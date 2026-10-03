#!/usr/bin/env python3
"""Round-4 independent verifier tests for ae4543d5 (guard-maintree-bash.py).

Written by the condukt verifier, not the implementing worker (CLAUDE.md 2(a)).
Each MustRefuse case is a write into the main tree that c8e4fd1e ALLOWS
(RED when written) although an EARLIER round of this guard refused it:

  * perl hex / octal escaped slash (backslash-x2f, backslash-057) opening the
    path: refused by cd130363, bac32610 and f05150a9 (a writing payload with
    no literal path); c8e4fd1e removes every backslash before extracting
    paths, so the escape turns into a bogus literal (x2fUsers/...) that is not
    under main and the no-literal-path refusal no longer fires.
  * find MAIN -exec touch {}/x: BSD and GNU find substitute {} inside an
    argument; refused by cd130363 only (claimed class: find -exec).
  * rsync SRC MAIN/ --exclude x: rsync takes options after the operands, and
    TARGET_LAST picks x as the destination; refused by cd130363 only
    (claimed class: rsync).

Ground-truth tests run the real tool inside the throwaway fixture to show the
command really changes main. MustAllow cases are anti-vacuity controls.

    python3 -m unittest scripts/test_guard_maintree_bash_ae4543d5_verify4.py
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
        self.tmp = Path(tempfile.mkdtemp(prefix="ae45v4-")).resolve()
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


    def status_main(self):
        r = subprocess.run(["git", "status", "--porcelain", "--untracked-files=all"],
                           cwd=self.main, env=_env(), capture_output=True, text=True)
        return r.stdout.strip()

    def reset_main(self):
        subprocess.run(["git", "checkout", "-q", "HEAD", "--", "."], cwd=self.main, env=_env())
        subprocess.run(["git", "clean", "-fdq"], cwd=self.main, env=_env())

    def perl_unlink(self, esc, root=None):
        # perl -e 'unlink "<esc>...main/f.txt"' with the leading slash escaped
        root = str(root or self.main)
        return Q.join(["perl -e ", "unlink " + "\"" + esc + root[1:] + "/f.txt\"", ""])

    def find_touch(self, root):
        return f"find {root} -maxdepth 0 -exec touch {{}}/p.txt {BS};"

    def rsync_into(self, dest):
        src = self.scratch / "src"
        src.mkdir(exist_ok=True)
        (src / "p.txt").write_text("x\n")
        return f"rsync -a {src}/ {dest}/ --exclude zz"


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
    """The real tools, run inside the fixture, change main."""

    def _changes_main(self, shell_cmd):
        self.f.reset_main()
        r = subprocess.run(["bash", "-c", shell_cmd], cwd=self.f.wt,
                           env=self.f.env(), capture_output=True, text=True)
        changed = self.f.status_main()
        self.f.reset_main()
        self.assertTrue(changed, f"ground truth void: {shell_cmd!r} rc={r.returncode} {r.stderr}")

    def test_perl_hex_escaped_slash_unlinks_main(self):
        self._changes_main(self.f.perl_unlink(BS + "x2f"))

    def test_perl_octal_escaped_slash_unlinks_main(self):
        self._changes_main(self.f.perl_unlink(BS + "057"))

    def test_find_exec_embedded_braces_writes_main(self):
        self._changes_main(self.f.find_touch(self.f.main))

    def test_rsync_trailing_option_writes_main(self):
        if shutil.which("rsync") is None:
            self.skipTest("rsync not installed")
        self._changes_main(self.f.rsync_into(self.f.main))


class MustRefuse(_Base):
    def test_perl_escaped_slash_regression(self):
        # refused by cd130363 / bac32610 / f05150a9
        self.refuse([self.f.perl_unlink(BS + "x2f"), self.f.perl_unlink(BS + "057")])

    def test_find_exec_braces_inside_an_argument(self):
        # refused by cd130363; claimed class find -exec
        self.refuse([self.f.find_touch(self.f.main)])

    def test_rsync_option_after_operands(self):
        # refused by cd130363; claimed class rsync
        self.refuse([self.f.rsync_into(self.f.main)])


class MustAllow(_Base):
    def test_controls(self):
        self.allow([
            self.f.find_touch(self.f.wt),
            self.f.rsync_into(self.f.wt),
            f"find {self.f.main} -name '*.txt' -exec grep -l a {{}} {BS};",
            f"perl -ne 'print if /a{BS}/b/' {self.f.main}/f.txt",
            self.f.perl_unlink("/", root=self.f.scratch),
        ])


if __name__ == "__main__":
    unittest.main()
