#!/usr/bin/env python3
"""Negative twins of test_guard_maintree_bash_merge_with_untracked.py (backlog 58bc638d).

A genuine two-parent merge with a pre-existing untracked path must be allowed
only when that path is unchanged. These tests do the same real merge but add a
second effect and assert the guard still REFUSES:

  (a) the same command also creates a new untracked file,
  (b) a pre-existing untracked file's content changes,
  (c) a tracked modification is left in main.

Written by a test author who did not implement the guard (CLAUDE.md 2.(a)).

    python3 scripts/test_guard_maintree_bash_merge_with_untracked_negative.py
"""
from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import test_guard_maintree_bash_merge_with_untracked as _pos  # noqa: E402

CMD = _pos.CMD


class MergeWithUntrackedStillRefusesOtherChanges(_pos.MergeWithPreexistingUntracked):
    SESSION = "sess-merge-untracked-neg-0001"

    # Do not re-run the positive tests of the imported base class here.
    test_red_merge_allowed_with_untracked_bare_repo_at_root = None
    test_red_merge_allowed_with_plain_untracked_file = None
    test_control_merge_allowed_with_clean_main = None

    def _setup_pre(self) -> str:
        self._side_commit()
        (self.main / "scratch.txt").write_text("untracked v1\n")
        self.assertTrue(self.status().strip(), "anti-vacuity: pre-existing untracked path")
        tid, _ = self.pre_ok(CMD)
        return tid

    def _assert_refused(self, r) -> None:
        self.assertNotEqual(r.returncode, 0,
                            "guard allowed a merge with an extra main-tree change")
        self.assertIn("Refused", r.stderr)

    def test_new_untracked_file_created_alongside_merge_refused(self) -> None:
        tid = self._setup_pre()
        self._merge_in_main()
        (self.main / "newly_created.txt").write_text("new\n")
        self.assertIn("newly_created.txt", self.status(), "anti-vacuity")
        self._assert_refused(self.post(tid, CMD))

    def test_preexisting_untracked_content_changed_alongside_merge_refused(self) -> None:
        tid = self._setup_pre()
        self._merge_in_main()
        (self.main / "scratch.txt").write_text("untracked v2 (changed)\n")
        self._assert_refused(self.post(tid, CMD))

    def test_tracked_modification_left_alongside_merge_refused(self) -> None:
        tid = self._setup_pre()
        self._merge_in_main()
        (self.main / "other.txt").write_text("tracked edit left behind\n")
        self.assertIn(" other.txt", self.status(), "anti-vacuity: tracked file modified")
        self._assert_refused(self.post(tid, CMD))


if __name__ == "__main__":
    prog = unittest.main(exit=False, verbosity=2)
    res = prog.result
    failed = len(res.failures) + len(res.errors)
    print("RESULT: %d passed, %d failed" % (res.testsRun - failed - len(res.skipped), failed))
    sys.exit(1 if failed else 0)
