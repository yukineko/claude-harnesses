#!/usr/bin/env python3
"""Round-3 independent verifier tests for ae4543d5 (guard-maintree-bash.py).

Written by the condukt verifier, not the implementing worker (CLAUDE.md 2(a)).
Each MustRefuse case is a write into the main tree that f05150a9 still ALLOWS
(RED when written). Ground-truth tests run the real tool inside the throwaway
fixture to show the write really lands in main (macOS BSD mktemp, bash).
MustAllow cases are anti-vacuity controls.

    python3 -m unittest scripts/test_guard_maintree_bash_ae4543d5_verify3.py
"""
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent
GUARD = SCRIPTS / "guard-maintree-bash.py"
Q = "'"
BQ = "`"
BS = "\\"


def _env(**extra):
    env = dict(os.environ)
    env.update(
        GIT_CONFIG_GLOBAL=os.devnull,
        GIT_CONFIG_NOSYSTEM="1",
        GIT_AUTHOR_NAME="t",
        GIT_AUTHOR_EMAIL="t@example.invalid",
        GIT_COMMITTER_NAME="t",
        GIT_COMMITTER_EMAIL="t@example.invalid",
    )
    env.update(extra)
    return env


def _git(cwd, *args):
    r = subprocess.run(["git", *args], cwd=cwd, env=_env(), capture_output=True, text=True)
    if r.returncode != 0:
        raise AssertionError(f"fixture void: git {args}: {r.stderr}")


class Fx:
    def __init__(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="ae45v3-")).resolve()
        self.main = self.tmp / "main"
        self.wt = self.tmp / "wt"
        self.scratch = self.tmp / "scratch"
        self.tmpdir = self.scratch / "tmpd"
        self.main.mkdir()
        self.tmpdir.mkdir(parents=True)
        _git(self.main, "init", "-q", "-b", "main")
        (self.main / "f.txt").write_text("a\n")
        _git(self.main, "add", "f.txt")
        _git(self.main, "commit", "-q", "-m", "seed")
        _git(self.main, "worktree", "add", "-q", "-b", "feat", str(self.wt))

    def close(self):
        shutil.rmtree(self.tmp, ignore_errors=True)

    def user_tmp(self):
        # macOS BSD `mktemp -t` uses the per-user temp dir (confstr), which is
        # also what the session's $TMPDIR is.
        r = subprocess.run(["getconf", "DARWIN_USER_TEMP_DIR"], capture_output=True, text=True)
        return os.path.realpath(r.stdout.strip()) if r.returncode == 0 and r.stdout.strip() else None

    def t_prefix(self):
        ut = self.user_tmp()
        return None if ut is None else os.path.relpath(str(self.main), ut) + "/x"

    def env(self):
        env = _env(CLAUDE_PROJECT_DIR=str(self.main), HOME=str(self.tmp),
                   TMPDIR=self.user_tmp() or str(self.tmpdir))
        env.pop("PWD", None)
        return env

    def run(self, command):
        payload = {"tool_name": "Bash", "tool_input": {"command": command},
                   "cwd": str(self.wt)}
        r = subprocess.run(
            [sys.executable, str(GUARD)],
            input=json.dumps(payload), capture_output=True, text=True,
            cwd=str(self.wt), env=self.env(),
        )
        return r.returncode, r.stderr

    def untracked_in_main(self):
        r = subprocess.run(["git", "status", "--porcelain", "--untracked-files=all"],
                           cwd=self.main, env=_env(), capture_output=True, text=True)
        return [l for l in r.stdout.splitlines() if l.startswith("??")]

    def clean_main(self):
        subprocess.run(["git", "clean", "-fdq"], cwd=self.main, env=_env())


class _Base(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.f = Fx()

    @classmethod
    def tearDownClass(cls):
        cls.f.close()

    def refuse(self, cmds):
        for c in cmds:
            with self.subTest(cmd=c):
                rc, _ = self.f.run(c)
                self.assertEqual(rc, 2, f"write into main ALLOWED: {c!r}")

    def allow(self, cmds):
        for c in cmds:
            with self.subTest(cmd=c):
                rc, err = self.f.run(c)
                self.assertEqual(rc, 0, f"non-main command REFUSED: {c!r}\n{err[:300]}")


class GroundTruth(_Base):
    """The real tools, run inside the fixture, write into main."""

    def _lands_in_main(self, shell_cmd):
        self.f.clean_main()
        r = subprocess.run(["bash", "-c", shell_cmd], cwd=self.f.wt,
                           env=self.f.env(), capture_output=True, text=True)
        hits = self.f.untracked_in_main()
        self.f.clean_main()
        self.assertTrue(hits, f"ground truth void: {shell_cmd!r} rc={r.returncode} {r.stderr}")

    def test_mktemp_p_template_escapes_its_dir(self):
        self._lands_in_main(f"mktemp -p {self.f.scratch} ../main/x.XXXX")

    def test_mktemp_t_prefix_escapes_tmpdir(self):
        if sys.platform != "darwin":
            self.skipTest("BSD mktemp -t semantics")
        if self.f.t_prefix() is None:
            self.skipTest("no DARWIN_USER_TEMP_DIR")
        self._lands_in_main(f"mktemp -t {self.f.t_prefix()}")

    def test_bare_mktemp_creates_in_main(self):
        self._lands_in_main(f"mktemp {self.f.main}/x.XXXX")

    def test_nested_escaped_backquote_runs(self):
        self._lands_in_main(f"echo {BQ}echo {BS}{BQ}touch {self.f.main}/n.txt{BS}{BQ}{BQ}")

    def test_cmd_from_substitution_runs(self):
        self._lands_in_main(f"CMD=$(echo touch {self.f.main}/c.txt); $CMD")


class MustRefuse(_Base):
    def test_mktemp_creating_in_main(self):
        M = self.f.main
        self.refuse([
            f"mktemp {M}/x.XXXX",
            f"mktemp -d {M}/x.XXXX",
            f"mktemp -p {M}",
            f"TMPDIR={M} mktemp",
            f"S=$(mktemp -p {M})",
            f"S=$(mktemp -d {M}/x.XXXX)",
        ])

    def test_mktemp_template_or_prefix_escaping_its_dir(self):
        S = self.f.scratch
        self.refuse([
            f"S=$(mktemp -p {S} ../main/x.XXXX); echo x > $S",
        ])
        if sys.platform == "darwin" and self.f.t_prefix():
            self.refuse([f"S=$(mktemp -t {self.f.t_prefix()}); echo x > $S"])

    def test_nested_escaped_backquote(self):
        M = self.f.main
        self.refuse([f"echo {BQ}echo {BS}{BQ}rm {M}/f.txt{BS}{BQ}{BQ}"])

    def test_program_variable_produced_by_a_command(self):
        # Residual list: an unknown program IS judged through "the literal
        # paths of the command that produced it". Through a variable it is not.
        M = self.f.main
        self.refuse([
            f"CMD=$(echo rm {M}/f.txt); $CMD",
            f"CMD={BQ}echo rm {M}/f.txt{BQ}; $CMD",
            f"CMD=$(echo rm {M}/f.txt); eval \"$CMD\"",
            f"printf -v CMD {Q}rm %s{Q} {M}/f.txt; $CMD",
            f"set -- rm {M}/f.txt; \"$@\"",
        ])

    def test_perl_escaped_slash_path(self):
        # `\/` is `/` inside a perl string / s///e replacement; the literal-path
        # extractor does not read it, and a stray `/e` counts as "a path".
        esc = str(self.f.main).replace("/", BS + "/")
        self.refuse([f"perl -e {Q}$_=\"a\"; s/a/unlink(\"{esc}{BS}/f.txt\")/e{Q}"])

    def test_ruby_pathname_and_nonliteral_mode(self):
        M = self.f.main
        self.refuse([
            f"ruby -e {Q}p=Pathname(\"{M}/p.txt\"); p.write(\"x\"){Q}",
            f"ruby -e {Q}m=\"w\"; File.open(\"{M}/p.txt\", m){Q}",
        ])


class MustAllow(_Base):
    def test_controls(self):
        S, W = self.f.scratch, self.f.wt
        self.allow([
            "mktemp -d",
            "S=$(mktemp -d); echo x > $S/f",
            f"mktemp -p {S}",
            f"S=$(mktemp -p {S} x.XXXX); echo x > $S",
            f"TMPDIR={S} mktemp",
            f"echo {BQ}echo hi{BQ}",
            f"echo {BQ}echo {BS}{BQ}echo hi{BS}{BQ}{BQ}",
            f"CMD=$(echo ls); $CMD {S}",
            f"printf -v X {Q}%s{Q} hi; echo $X",
            f"ruby -e {Q}p=Pathname(\"{W}/p.txt\"); p.write(\"x\"){Q}",
            f"perl -e {Q}print \"{BS}/tmp{BS}/x\"{Q}",
        ])


if __name__ == "__main__":
    unittest.main()
