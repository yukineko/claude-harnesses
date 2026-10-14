#!/usr/bin/env python3
"""backlog 6b5971e4: guard-maintree-bash refuses `git merge --abort` on main.

Claim (observed 2026-10-04): after a conflicted `git merge --no-ff <branch>` on
main, `git merge --abort` is refused by the PostToolUse guard as "modified this
project's MAIN working tree outside integration", although aborting one's own
conflicted merge is part of integration (CLAUDE.md 8) and leaves main exactly
at HEAD.

The test drives the real guard over a real conflicted merge in a fixture repo:
Pre snapshot (merge in progress) -> `git merge --abort` -> Post. A control
asserts the abort really restored main to a clean HEAD, so the expected allow
is not vacuous.

RED repro: python3 -m pytest --runxfail scripts/test_backlog_6b5971e4.py
"""
from __future__ import annotations

import unittest

from test_guard_maintree_bash_observe import ObserveGuardBase, git


class MergeAbortOnMain(ObserveGuardBase):
    def _conflicted_merge(self) -> str:
        git(self.wt, "checkout", "-q", "wtbranch")
        (self.wt / "tracked.txt").write_text("branch side\n")
        git(self.wt, "commit", "-q", "-am", "branch side")
        (self.main / "tracked.txt").write_text("main side\n")
        git(self.main, "commit", "-q", "-am", "main side")
        head = git(self.main, "rev-parse", "HEAD").stdout.strip()
        proc = git(self.main, "merge", "--no-ff", "wtbranch", check=False)
        self.assertNotEqual(proc.returncode, 0, "fixture: the merge must conflict")
        self.assertTrue((self.main / ".git" / "MERGE_HEAD").exists())
        return head

    @unittest.expectedFailure
    def test_merge_abort_of_own_conflicted_merge_is_allowed(self) -> None:
        head = self._conflicted_merge()
        tid = self._next_id()
        pre = self.run_hook("PreToolUse", tid, command="git merge --abort")
        self.assertEqual(pre.returncode, 0, pre.stderr)
        git(self.main, "merge", "--abort")
        # control: main is back exactly at HEAD, clean, no merge in progress
        self.assertEqual(git(self.main, "rev-parse", "HEAD").stdout.strip(), head)
        self.assertEqual(git(self.main, "status", "--porcelain").stdout, "")
        self.assertFalse((self.main / ".git" / "MERGE_HEAD").exists())
        post = self.run_hook("PostToolUse", tid, command="git merge --abort")
        self.assertEqual(
            post.returncode,
            0,
            "aborting an in-progress merge back to a clean HEAD is integration, "
            "not a main-tree modification; guard said: %s" % post.stderr,
        )


if __name__ == "__main__":
    unittest.main()
