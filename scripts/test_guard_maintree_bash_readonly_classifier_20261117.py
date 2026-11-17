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
    "git status", "git log --oneline -5", "git -C /tmp/x status --porcelain=v2",
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


if __name__ == "__main__":
    unittest.main()
