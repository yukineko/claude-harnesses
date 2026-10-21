#!/usr/bin/env python3
"""backlog 234011a6: the e033c406 deny-ledger tests depend on a short TMPDIR.

Claim (observed 2026-10-03): with a long TMPDIR, tests in
test_deny_ledger_e033c406*.py fail, because deny_ledger.record_deny truncates
`raw` to RAW_LIMIT (120) characters and the tests assert the full
"Write <fixture path>" string appears in the ledger entry / refusal quote.

Measured at triage (main e1aca620): the RETRY MATCHER is not affected (it
matches on target_abs / raw_target, and the refusal still names the full
target); the failures are the tests asserting on the truncated `raw` field.
So the defect is test fragility: the suite's verdict depends on where the
caller's TMPDIR lives.

This test runs the affected test cases in a subprocess with a long (but real)
TMPDIR and asserts they pass, as they do with a short one.

RED repro: python3 -m pytest --runxfail scripts/test_backlog_234011a6.py
"""
from __future__ import annotations

import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent

CASES = [
    "test_deny_ledger_e033c406.py::DenyIsRecorded::test_edit_deny_writes_a_ledger_entry_under_home",
    "test_deny_ledger_e033c406.py::RetryIsRefused::test_retry_naming_the_denied_target_is_refused_quoting_the_deny",
    "test_deny_ledger_e033c406_verify.py::RetryPrevention::test_retry_naming_target_is_denied_noninteractive_quoting_deny",
]


class DenyLedgerTestsIndependentOfTmpdir(unittest.TestCase):
    def _run(self, tmpdir: str) -> subprocess.CompletedProcess:
        env = dict(os.environ, TMPDIR=tmpdir)
        return subprocess.run(
            [sys.executable, "-m", "pytest", "-q", "-p", "no:cacheprovider",
             *[str(HERE / c) for c in CASES]],
            cwd=str(HERE), env=env, capture_output=True, text=True, timeout=600,
        )

    def setUp(self) -> None:
        self.base = Path(os.path.realpath(tempfile.mkdtemp(prefix="bl-", dir="/tmp")))
        self.addCleanup(shutil.rmtree, self.base, True)

    def test_control_short_tmpdir_passes(self) -> None:
        proc = self._run(str(self.base))
        self.assertEqual(proc.returncode, 0, proc.stdout[-3000:] + proc.stderr[-2000:])

    def test_long_tmpdir_passes_too(self) -> None:
        long_dir = self.base / ("long-scratchpad-" + "x" * 100)
        long_dir.mkdir()
        proc = self._run(str(long_dir))
        self.assertEqual(
            proc.returncode,
            0,
            "deny-ledger tests fail only because TMPDIR is long:\n"
            + proc.stdout[-3000:] + proc.stderr[-2000:],
        )


if __name__ == "__main__":
    unittest.main()
