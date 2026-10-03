#!/usr/bin/env python3
"""Round-2 independent verifier tests for ae4543d5 (guard-maintree-bash.py).

Written by the condukt verifier, not the implementing worker (CLAUDE.md 2(a)).
Each MustRefuse case is a write into the main tree that bac32610 still ALLOWS;
each was RED when written. MustAllow cases are anti-vacuity controls so a fix
cannot pass by refusing every glob / variable / eval.

The hook payload carries `cwd` = a linked worktree, as Claude Code sends it.

    python3 -m unittest scripts/test_guard_maintree_bash_ae4543d5_verify2.py
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
        self.tmp = Path(tempfile.mkdtemp(prefix="ae45v2-")).resolve()
        self.main = self.tmp / "main"
        self.wt = self.tmp / "wt"
        self.scratch = self.tmp / "scratch"
        self.main.mkdir()
        (self.scratch / "sub").mkdir(parents=True)
        _git(self.main, "init", "-q", "-b", "main")
        (self.main / "f.txt").write_text("a\n")
        _git(self.main, "add", "f.txt")
        _git(self.main, "commit", "-q", "-m", "seed")
        _git(self.main, "worktree", "add", "-q", "-b", "feat", str(self.wt))

    def close(self):
        shutil.rmtree(self.tmp, ignore_errors=True)

    def run(self, command):
        payload = {"tool_name": "Bash", "tool_input": {"command": command},
                   "cwd": str(self.wt)}
        env = _env(CLAUDE_PROJECT_DIR=str(self.main), HOME=str(self.tmp))
        env.pop("PWD", None)
        r = subprocess.run(
            [sys.executable, str(GUARD)],
            input=json.dumps(payload), capture_output=True, text=True,
            cwd=str(self.wt), env=env,
        )
        return r.returncode, r.stderr


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


class MustRefuse(_Base):
    def test_fixture_dotdot_after_glob_really_reaches_main(self):
        # Ground truth, so the refuse cases below are not hypothetical.
        import glob
        hits = [os.path.realpath(p) for p in
                glob.glob(f"{self.f.scratch}/*/../../main/f.txt")]
        self.assertIn(str(self.f.main / "f.txt"), hits)

    def test_dotdot_after_unresolvable_component(self):
        # The literal prefix (<scratch>/) is disjoint from main, but `..` after
        # the glob / unknown component climbs back out and into main.
        S = self.f.scratch
        self.refuse([
            f"rm {S}/*/../../main/f.txt",
            f"echo x > {S}/*/../../main/p.txt",
            f"rm {S}/$X/../../main/f.txt",
            f"for d in {S}/a {S}/b; do rm $d/../../main/f.txt; done",
        ])

    def test_same_command_assigned_program_text(self):
        # The docstring says variables assigned earlier in the SAME command are
        # tracked and expanded, and only a CMD "not assigned in the command" is
        # an unknown-program residual. Expanding CMD to the one word
        # "rm <main>/f.txt" and taking its basename as the program loses it.
        M = self.f.main
        self.refuse([
            f"CMD=\"rm {M}/f.txt\"; $CMD",
            f"CMD=\"rm {M}/f.txt\"; eval \"$CMD\"",
            f"CMD=\"rm {M}/f.txt\"; sh -c \"$CMD\"",
        ])

    def test_eval_of_substitution_naming_main(self):
        # done_criteria: eval / sh -c payloads judged recursively; anything that
        # cannot be expanded resolves to refuse. The substitution's output is
        # unknown, and the command text names a main path literally. Refused
        # by cd130363, allowed by bac32610 (regression of the relaxation).
        M = self.f.main
        self.refuse([
            f"eval \"$(echo rm {M}/f.txt)\"",
            f"eval \"$(printf {Q}rm %s{Q} {M}/f.txt)\"",
            f"sh -c \"$(echo rm {M}/f.txt)\"",
        ])


class MustAllow(_Base):
    def test_controls(self):
        S, W = self.f.scratch, self.f.wt
        self.allow([
            f"echo x > {S}/sub*/out.txt",
            f"rm {S}/*/x.txt",
            f"rm {S}/$X/x.txt",
            f"for d in {S}/a {S}/b; do rm $d/x.txt; done",
            f"CMD=\"rm {W}/x.txt\"; $CMD",
            f"CMD=\"rm {S}/x.txt\"; eval \"$CMD\"",
            "eval \"$(brew shellenv)\"",
            "eval \"$(ssh-agent -s)\"",
            f"eval \"$(echo rm {S}/x.txt)\"",
        ])


if __name__ == "__main__":
    unittest.main()
