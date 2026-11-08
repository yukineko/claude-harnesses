#!/usr/bin/env python3
"""RED repro for GitHub issue #1551: the pre-commit hook records no per-phase
timing, while the pre-push hook appends one JSON line per run to a timing log in
the git common dir and prints a `pre-push timing:` summary.

Contract pinned here (reading (a), missing observability):
  * one JSON line per run appended to <git-common-dir>/precommit-timing.jsonl;
  * one stderr line `pre-commit timing: ...; total Ns (exit RC)`;
  * observability only: the hook's exit code does not change (allow and block).

Fixture: reuses HookHarness from test_precommit_hook.py (a throwaway repo with a
copy of the hook and STUB scanners), so the real scanners never run.

Run: `python3 -m pytest scripts/test_precommit_timing.py`. The RED tests carry
`unittest.expectedFailure`; set PRECOMMIT_TIMING_RED=1 to run them unmarked and
observe the real failure.
"""

from __future__ import annotations

import json
import os
import re
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from test_precommit_hook import HookHarness, HookTestCase, _which  # noqa: E402

_SUMMARY_RE = re.compile(
    r"^pre-commit timing: .*total (?:\d+|\?)s \(exit (?P<rc>\d+)\)$", re.MULTILINE
)


def red_1551(fn):
    """RED for #1551. PRECOMMIT_TIMING_RED=1 runs the test unmarked."""
    if os.environ.get("PRECOMMIT_TIMING_RED") == "1":
        return fn
    return unittest.expectedFailure(fn)


class TimedHarness(HookHarness):
    def __init__(self, **kw):
        super().__init__(**kw)
        link = self.bindir / "date"
        if not link.exists():
            os.symlink(_which("date"), link)
        self.log_path = self.root / ".git" / "precommit-timing.jsonl"


class PrecommitTiming(HookTestCase):
    def harness(self, **kw):
        h = TimedHarness(**kw)
        self.addCleanup(h.cleanup)
        return h

    def test_control_exit_codes_unchanged(self):
        """Control (passes today): clean -> 0, a failing scanner -> non-zero."""
        self.assertEqual(self.harness().run().returncode, 0)
        self.assertNotEqual(
            self.harness(exits={"check-fail-open.py": 1}).run().returncode, 0
        )

    @red_1551
    def test_allow_appends_one_json_line_and_prints_summary(self):
        h = self.harness()
        proc = h.run()
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertTrue(
            h.log_path.is_file(), "no precommit-timing.jsonl written at %s" % h.log_path
        )
        lines = [ln for ln in h.log_path.read_text().splitlines() if ln]
        self.assertEqual(len(lines), 1, lines)
        self.assertEqual(json.loads(lines[0]).get("exit"), 0)
        m = _SUMMARY_RE.search(proc.stderr)
        self.assertIsNotNone(m, "no 'pre-commit timing:' summary on stderr:\n" + proc.stderr)
        self.assertEqual(m.group("rc"), "0")

    @red_1551
    def test_block_records_timing_and_keeps_nonzero_exit(self):
        h = self.harness(exits={"check-fail-open.py": 1})
        proc = h.run()
        self.assertNotEqual(proc.returncode, 0)
        self.assertTrue(h.log_path.is_file(), "no precommit-timing.jsonl on a block")
        rec = json.loads([ln for ln in h.log_path.read_text().splitlines() if ln][-1])
        self.assertEqual(rec.get("exit"), proc.returncode)
        m = _SUMMARY_RE.search(proc.stderr)
        self.assertIsNotNone(m, "no 'pre-commit timing:' summary on stderr:\n" + proc.stderr)
        self.assertEqual(m.group("rc"), str(proc.returncode))


if __name__ == "__main__":
    unittest.main()
