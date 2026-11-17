#!/usr/bin/env python3
"""Pins command_provably_read_only() against the independent verifier's probes
(2026-11-17, backlog dc97fc2d). Every NOT_PROVABLE case was shown to be
classified True by the first version; each must stay False. These are the
author's own regression pins of those probes, not an independent oracle.

    python3 -m pytest scripts/test_guard_maintree_bash_readonly_classifier_20261117.py
"""
from __future__ import annotations

import importlib.util
import unittest
from pathlib import Path

_spec = importlib.util.spec_from_file_location(
    "guard_maintree_bash", Path(__file__).resolve().parent / "guard-maintree-bash.py")
gm = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(gm)

NOT_PROVABLE = [
    "file -C -m x",
    "file -m x f",
    "git diff --outpu=o", "git diff --out=o", "git log --out=o",
    "git show --outp=o", "git diff --ext", "git diff --textc", "git log --text",
    "git diff --output=o", "git diff --ext-diff", "git log --textconv",
    "git cat-file --filters HEAD:f", "git blame --contents f f",
    "git diff -O orderfile", "git ls-files --debug",
    "git -c core.pager=x log", "git status > out", "git status $(x)",
    "tail -f log", "ls --unknown", "grep -f pats f", "cat f > g",
    "git push", "git commit -m x", "sed -i s/a/b/ f", "echo hi; rm f",
]
PROVABLE = [
    "git status", "git log --oneline -5",
    "git diff --stat HEAD~1", "git rev-parse --show-toplevel",
    "ls -la", "cat a b", "grep -n foo f | head -5", "git log -n5 -- path",
    "git status && git log --oneline", "file -b f", "echo -n hi",
]


class Classifier(unittest.TestCase):
    def test_probes_are_not_provable(self) -> None:
        for c in NOT_PROVABLE:
            with self.subTest(c=c):
                self.assertFalse(gm.command_provably_read_only(c), c)

    def test_plain_reads_are_provable(self) -> None:
        for c in PROVABLE:
            with self.subTest(c=c):
                self.assertTrue(gm.command_provably_read_only(c), c)


class GitDashC(unittest.TestCase):
    """User ruling 2026-11-18: `git -C <path>` is provable only for a checkout
    of THIS repository (same git common dir; linked worktrees count)."""

    @classmethod
    def setUpClass(cls) -> None:
        import os
        import subprocess
        import tempfile
        cls.tmp = Path(tempfile.mkdtemp(prefix="ro-dashc-")).resolve()
        env = {**os.environ, "GIT_AUTHOR_NAME": "t", "GIT_AUTHOR_EMAIL": "t@t",
               "GIT_COMMITTER_NAME": "t", "GIT_COMMITTER_EMAIL": "t@t"}

        def git(cwd, *a):
            subprocess.run(["git", "-C", str(cwd), *a], check=True, env=env,
                           capture_output=True)
        cls.main = cls.tmp / "main"
        cls.main.mkdir()
        git(cls.main, "init", "-q", "-b", "main")
        git(cls.main, "-c", "commit.gpgsign=false", "commit", "-q", "--allow-empty", "-m", "i")
        cls.wt = cls.tmp / "wt"
        git(cls.main, "worktree", "add", "-q", "-b", "side", str(cls.wt))
        cls.other = cls.tmp / "other"
        cls.other.mkdir()
        git(cls.other, "init", "-q", "-b", "main")
        cls.common = cls.main / ".git"

    @classmethod
    def tearDownClass(cls) -> None:
        import shutil
        shutil.rmtree(cls.tmp, ignore_errors=True)

    def ro(self, cmd: str, cwd=None) -> bool:
        return gm.command_provably_read_only(
            cmd, cwd=str(cwd or self.main), common=self.common)

    def test_linked_worktree_of_this_repo_is_provable(self) -> None:
        self.assertTrue(self.ro(f"git -C {self.wt} status --porcelain=v2"))

    def test_relative_path_resolves_against_cwd(self) -> None:
        self.assertTrue(self.ro("git -C ../wt log --oneline", cwd=self.main))

    def test_primary_checkout_is_provable(self) -> None:
        self.assertTrue(self.ro(f"git -C {self.main} status"))

    def test_unrelated_repo_is_not_provable(self) -> None:
        self.assertFalse(self.ro(f"git -C {self.other} status"))

    def test_nonexistent_path_is_not_provable(self) -> None:
        self.assertFalse(self.ro(f"git -C {self.tmp}/nope status"))

    def test_non_repo_dir_is_not_provable(self) -> None:
        self.assertFalse(self.ro(f"git -C {self.tmp} status"))

    def test_without_context_any_dash_c_is_not_provable(self) -> None:
        self.assertFalse(gm.command_provably_read_only(f"git -C {self.wt} status"))

    def test_second_dash_c_is_not_provable(self) -> None:
        self.assertFalse(self.ro(f"git -C {self.wt} -C {self.other} status"))

    def test_plain_git_without_dash_c_unchanged(self) -> None:
        self.assertTrue(gm.command_provably_read_only("git status"))


if __name__ == "__main__":
    unittest.main()
