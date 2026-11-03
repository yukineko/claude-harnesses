#!/usr/bin/env python3
"""guard-maintree-bash refuses a genuine two-parent merge when main already had
an UNRELATED tracked modification at Pre that is byte-identical at Post
(file-script-findings F-E, observed 2026-11-03: `condukt worktree merge`
c23cc705 (parents 800b3318, 6f7136e8) was refused with
"HEAD 800b33189836 -> c23cc705e421 (not a merge / fast-forward)").

only_unchanged_untracked() (scripts/guard-maintree-bash.py) tolerates a
pre-existing dirty path only when it is untracked ("? "); a tracked
modification that is the same at Pre and Post is treated as a change, so
integration_head() is never asked about the new HEAD and the REFUSE message
blames "not a merge / fast-forward" for a two-parent merge. The twin
test_guard_maintree_bash_merge_with_untracked.py covers the untracked case.

Expected: a real merge with an unchanged pre-existing tracked modification is
allowed (nothing in main was changed BY the command).

    python3 scripts/test_guard_maintree_bash_merge_with_preexisting_tracked_mod.py
"""
from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import test_guard_maintree_bash_merge_with_untracked as _pos  # noqa: E402

CMD = _pos.CMD


class MergeWithPreexistingTrackedMod(_pos.MergeWithPreexistingUntracked):
    SESSION = "sess-merge-trackedmod-0001"

    test_red_merge_allowed_with_untracked_bare_repo_at_root = None
    test_red_merge_allowed_with_plain_untracked_file = None
    test_control_merge_allowed_with_clean_main = None

    def test_red_merge_allowed_with_unchanged_preexisting_tracked_modification(self) -> None:
        self._side_commit()  # touches tracked.txt only
        (self.main / "other.txt").write_text("o1 edited by another session\n")
        self.assertIn("other.txt", self.status(), "anti-vacuity: pre-existing tracked mod")
        tid, _ = self.pre_ok(CMD)
        self._merge_in_main()  # asserts a two-parent HEAD
        self.assertIn("other.txt", self.status(), "anti-vacuity: modification still present at Post")
        r = self.post(tid, CMD)
        self.assertEqual(r.returncode, 0,
                         "genuine two-parent merge refused (rc=%d): %s" % (r.returncode, r.stderr))


if __name__ == "__main__":
    prog = unittest.main(exit=False, verbosity=2)
    res = prog.result
    failed = len(res.failures) + len(res.errors)
    print("RESULT: %d passed, %d failed" % (res.testsRun - failed - len(res.skipped), failed))
    sys.exit(1 if failed else 0)
