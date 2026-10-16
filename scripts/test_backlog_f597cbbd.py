#!/usr/bin/env python3
"""backlog f597cbbd: the observing main-tree guard (scripts/guard-maintree-bash.py)
refused an unrelated Bash call because ANOTHER session fast-forward-merged a
worktree branch into main during that call (observed 2026-10-04: dfd7357d
`merge flow-specdoc: Fast-forward`, attributed to session 64554c4d's call).

The guard's own contract allows a fast-forward integration at Post ("HEAD moved,
tree clean, and the new HEAD ... is contained in a ref other than the branch
main has checked out"). That containment test is the only FF evidence it has,
so once the merged branch is deleted (the repo's merge-then-delete rule) the
FF reads as "not a merge / fast-forward" and is refused — and the refusal lands
on whichever call happened to straddle it.

Expected (claimed) property: an unrelated call during which main receives a
completed fast-forward integration is not refused.

RED repro:  python3 -m pytest --runxfail scripts/test_backlog_f597cbbd.py
"""
from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from test_guard_maintree_bash_observe import ObserveGuardBase, git  # noqa: E402


class ForeignFastForwardDuringCall(ObserveGuardBase):
    def _foreign_ff_during_unrelated_call(self, delete_branch: bool):
        (self.wt / "ff.txt").write_text("ff\n")
        git(self.wt, "add", "ff.txt")
        git(self.wt, "commit", "-q", "-m", "ff")
        head_before = git(self.main, "rev-parse", "HEAD").stdout.strip()

        # This session's call: `true` in its own worktree.
        tid, _ = self.pre_ok("true", cwd=self.wt)
        # Meanwhile another session integrates: FF merge into main, then the
        # merged branch is cleaned up.
        git(self.main, "merge", "-q", "--ff-only", "wtbranch")
        if delete_branch:
            git(self.main, "worktree", "remove", "--force", str(self.wt))
            git(self.main, "branch", "-q", "-D", "wtbranch")
        post = self.run_hook("PostToolUse", tid, "true", cwd=self.main)

        # anti-vacuity: main really received a fast-forward and is clean
        head_after = git(self.main, "rev-parse", "HEAD").stdout.strip()
        self.assertNotEqual(head_before, head_after)
        parents = git(self.main, "rev-list", "--parents", "-n1", head_after).stdout.split()
        self.assertEqual(parents[1:], [head_before], "anti-vacuity: not a fast-forward")
        self.assertEqual(self.status(), "", "anti-vacuity: main tree not clean")
        return post

    def test_control_ff_with_branch_still_present_is_allowed(self) -> None:
        self.assert_allowed(self._foreign_ff_during_unrelated_call(delete_branch=False))

    @unittest.expectedFailure
    def test_ff_whose_branch_was_deleted_is_not_refused(self) -> None:
        self.assert_allowed(self._foreign_ff_during_unrelated_call(delete_branch=True))


if __name__ == "__main__":
    unittest.main()
