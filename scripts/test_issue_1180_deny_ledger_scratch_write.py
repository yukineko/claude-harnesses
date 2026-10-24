#!/usr/bin/env python3
"""Issue #1180: the retry window refuses a Write to an unrelated scratch file.

scripts/deny_ledger.py check_retry() refuses ANY guarded call whose tool_input
mentions a denied main-tree path for 25 calls / 20 minutes. A Write to a
scratch file outside every repository, whose CONTENT merely names the denied
path (a note, a report), writes nothing into main, yet it is refused with
"... reaches the same target ... by another spelling" (deny_ledger.py RETRY).

Contract asserted (issue #1180, narrowing to write-shaped calls): a Write whose
file_path is a scratch file outside the repository is allowed even when its
content names the denied path. Control: a Write to an unrelated scratch file
that does not name the path is allowed.

NOTE: the deny_ledger.py docstring ("a SUBSTRING test on purpose ... reads
included") and test_deny_ledger_e033c406.py
test_retry_through_write_content_is_refused (a WORKTREE file) pin the current
behaviour. Fixing this is a contract change.

    python3 -m pytest scripts/test_issue_1180_deny_ledger_scratch_write.py

Written by an independent test author (CLAUDE.md 2.(a)); not by the implementer.
"""
from __future__ import annotations

import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from test_deny_ledger_e033c406 import EDIT, SID, _Fixture  # noqa: E402


class ScratchWriteNamingDeniedPath(_Fixture):
    def _scratch_write(self, content: str):
        scratch = os.path.join(self.tmp, "scratch")
        os.makedirs(scratch, exist_ok=True)
        return self.run_hook(EDIT, {
            "hook_event_name": "PreToolUse", "session_id": SID,
            "tool_name": "Write", "cwd": self.wt,
            "tool_input": {"file_path": os.path.join(scratch, "notes.md"),
                           "content": content}}, cwd=self.wt)

    def test_control_unrelated_scratch_write_is_allowed(self) -> None:
        self.deny_main()
        r = self._scratch_write("nothing about main here\n")
        self.assertEqual(r.returncode, 0, r.stderr)

    def test_scratch_write_whose_content_names_the_denied_path_is_allowed(self) -> None:
        self.deny_main()
        r = self._scratch_write(f"report: the edit of {self.target} was refused\n")
        self.assertFalse(os.path.exists(os.path.join(self.tmp, "scratch", "notes.md")),
                         "anti-vacuity: the hook is consulted before the write")
        self.assertEqual(r.returncode, 0,
                         "a scratch-file note naming the path is not a retry; "
                         f"stderr={r.stderr!r}")


if __name__ == "__main__":
    unittest.main()
