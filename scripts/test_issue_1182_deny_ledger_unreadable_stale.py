#!/usr/bin/env python3
"""Issue #1182: a stale UNREADABLE (mode 000) deny ledger lets Stop allow.

scripts/deny_ledger.py _read_bounded(): once a bad ledger is older than
RETRY_WINDOW_SECS it is moved aside and salvaged. When the file is unreadable
(not corrupt), _read_bytes raises, the salvage is `[]` with the note
"dropped unparseable line(s): all (unreadable)", and stop_check() sees no deny
at all, so a main-tree target that was refused and then changed no longer
blocks the stop. Unreadable is "cannot determine", not "no prior refusals"
(CLAUDE.md 3).

Control: the same ledger, readable and backdated the same way, blocks.

Contract asserted: with the ledger at mode 000 and backdated 25 minutes, Stop
(scripts/stop-verify-worktree.py) still blocks (exit 2).

    python3 -m pytest scripts/test_issue_1182_deny_ledger_unreadable_stale.py

Written by an independent test author (CLAUDE.md 2.(a)); not by the implementer.
"""
from __future__ import annotations

import os
import sys
import time
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from test_deny_ledger_e033c406 import _Fixture  # noqa: E402


class UnreadableStaleLedger(_Fixture):
    def _deny_change_and_backdate(self) -> None:
        self.deny_main()
        self.assertTrue(os.path.exists(self.ledger), "anti-vacuity: deny was recorded")
        with open(self.target, "a") as f:
            f.write("changed after the refusal\n")
        old = time.time() - 25 * 60
        os.utime(self.ledger, (old, old))

    def test_control_readable_stale_ledger_blocks_stop(self) -> None:
        self._deny_change_and_backdate()
        r = self.stop()
        self.assertEqual(r.returncode, 2, r.stderr)

    def test_unreadable_stale_ledger_still_blocks_stop(self) -> None:
        self._deny_change_and_backdate()
        os.chmod(self.ledger, 0)
        self.addCleanup(lambda: [os.chmod(os.path.join(d, n), 0o600)
                                 for d in [os.path.dirname(self.ledger)]
                                 for n in os.listdir(d)])
        with self.assertRaises(PermissionError, msg="anti-vacuity: ledger is unreadable"):
            open(self.ledger, "rb").close()
        r = self.stop()
        self.assertEqual(r.returncode, 2,
                         "an unreadable ledger is not 'no prior denies'; "
                         f"stop stderr={r.stderr!r}")


if __name__ == "__main__":
    unittest.main()
