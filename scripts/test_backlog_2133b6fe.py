#!/usr/bin/env python3
"""Closure regression for backlog 2133b6fe.

2133b6fe: `check-fail-open.py --ratchet` (which scores the empty-collection
fallback class, e.g. `Err(_) => Vec::new()`) was not called by ANY hook, so a
new intrusion of that class was measured but never stopped. Completion
condition: pick one of the options and prove a NEW intrusion is detected.
The user chose (b), a diff-scoped ratchet (check-fail-open-diff.py).

Pinned here:
  1. .githooks/pre-commit actively runs check-fail-open-diff.py through its
     blocking `run` helper (wired, not just present on disk);
  2. the script, pointed at a fixture repo, exits 1 for a commit
     that stages ONE new empty-collection fallback in a crate source file,
     and 0 for the same file staged without it (control).
"""

from __future__ import annotations

import os
import re
import subprocess
import sys
import tempfile
import unittest

SCRIPTS = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(SCRIPTS)
GATE = os.path.join(SCRIPTS, "check-fail-open-diff.py")
PRECOMMIT = os.path.join(REPO, ".githooks", "pre-commit")

CLEAN = """pub fn read(p: &std::path::Path) -> Vec<u8> {
    match std::fs::read(p) {
        Ok(b) => b,
        Err(e) => panic!("{e}"),
    }
}
"""
SWALLOW = CLEAN.replace('Err(e) => panic!("{e}"),', "Err(_) => Vec::new(),")


def _git(repo: str, *args: str) -> None:
    subprocess.run(["git", *args], cwd=repo, check=True, capture_output=True)


class Backlog2133b6fe(unittest.TestCase):
    def setUp(self) -> None:
        self.repo = tempfile.mkdtemp(prefix="bl-2133b6fe.")
        self.addCleanup(lambda: subprocess.run(["rm", "-rf", self.repo]))
        _git(self.repo, "init", "-q")
        _git(self.repo, "config", "user.email", "t@t")
        _git(self.repo, "config", "user.name", "t")
        self.src = os.path.join(self.repo, "crates", "a", "src", "lib.rs")
        os.makedirs(os.path.dirname(self.src))
        with open(self.src, "w") as f:
            f.write(CLEAN)
        _git(self.repo, "add", "-A")
        _git(self.repo, "commit", "-qm", "base")

    def _stage(self, body: str) -> int:
        with open(self.src, "w") as f:
            f.write(body)
        _git(self.repo, "add", "-A")
        # The hook runs the script with no args; it then judges the repo the
        # script lives in (fo.REPO). Point it at the fixture repo explicitly.
        p = subprocess.run([sys.executable, GATE, "--repo", self.repo], cwd=self.repo, capture_output=True, text=True)
        return p.returncode

    def test_new_empty_collection_fallback_is_blocked(self):
        self.assertEqual(self._stage(SWALLOW), 1)

    def test_control_clean_edit_passes(self):
        self.assertEqual(self._stage(CLEAN + "// touched\n"), 0)

    def test_precommit_runs_the_diff_ratchet(self):
        with open(PRECOMMIT, encoding="utf-8") as f:
            text = f.read()
        self.assertRegex(
            text, re.compile(r"^run check-fail-open-diff\.py\s+\S+", re.M),
            "pre-commit must run check-fail-open-diff.py through its blocking run helper",
        )


if __name__ == "__main__":
    unittest.main()
