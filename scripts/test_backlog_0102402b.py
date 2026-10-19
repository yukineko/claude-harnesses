#!/usr/bin/env python3
"""Repro test for backlog 0102402b: a claim.rs test-module comment describes the
OLD head signal in the present tense.

crates/condukt/src/claim.rs (test section "the reap gate's head signal must be
RUN-scoped") says "Signal 1 is today `progress::git_head_signal(&repo_root(cwd))`
— the WHOLE project repo's HEAD", while claim_progress now computes signal 1 with
`run_worktree_head_signal(&rs)` (the run's recorded worktrees). The doc comment on
claim_progress itself already says "used to be". Comment vs code (CLAUDE.md 4).

The test pins both halves: the code really uses run_worktree_head_signal (control,
so the test cannot pass by the code regressing), and no comment asserts the
repo-wide signal "is today" the one in use. Open; expectedFailure.
"""

import re
import unittest
from pathlib import Path

CLAIM = Path(__file__).resolve().parent.parent / "crates" / "condukt" / "src" / "claim.rs"


class ClaimCommentMatchesCode(unittest.TestCase):
    def test_control_code_uses_run_scoped_signal(self):
        src = CLAIM.read_text()
        self.assertRegex(src, r"run_worktree_head_signal\(&rs\)")

    @unittest.expectedFailure
    def test_no_present_tense_repo_wide_claim(self):
        lines = CLAIM.read_text().splitlines()
        stale = [
            "%d: %s" % (i, l.strip())
            for i, l in enumerate(lines, 1)
            if l.lstrip().startswith("//")
            and re.search(r"is today `progress::git_head_signal\(&repo_root\(cwd\)\)`", l)
        ]
        self.assertEqual(stale, [], "comment states the replaced signal as current")


if __name__ == "__main__":
    unittest.main()
