#!/usr/bin/env python3
"""RED acceptance tests for GitHub #1551: .githooks/pre-commit per-phase timing.

Pins (a) "missing observability": pre-push appends one JSON line per run to
<git-common-dir>/prepush-timing.jsonl and prints a `pre-push timing:` summary;
pre-commit must do the same with precommit-timing.jsonl / `pre-commit timing:`.

Run:  python3 -m pytest scripts/test_precommit_timing.py

Reuses HookHarness (stub-scanner throwaway repo) from test_precommit_hook.py;
nothing touches the real repo, scanners or index.

Controls (normal tests, pass today): a clean run exits 0; a blocking run still
blocks with the same rc.  They prove the harness itself works, so a feature
test failing means "feature missing", not "setup broken".

The remaining tests pin the feature (GitHub #1551). They were strict xfails
while RED; the markers were removed when the feature landed.
"""

from __future__ import annotations

import json
import re
import sys
import unittest
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent))
from test_precommit_hook import (  # noqa: E402
    EXPECTED_SCANNERS,
    LABELS,
    HookHarness,
)

BLOCKER = "check-doc-duplication.py"
LOG_NAME = "precommit-timing.jsonl"


class TimingCase(unittest.TestCase):
    def harness(self, **kwargs):
        h = HookHarness(**kwargs)
        self.addCleanup(h.cleanup)
        return h

    @staticmethod
    def log_path(h):
        return h.root / ".git" / LOG_NAME

    def read_lines(self, h):
        p = self.log_path(h)
        self.assertTrue(p.is_file(), "%s was not written under the git common dir" % LOG_NAME)
        return [json.loads(ln) for ln in p.read_text().splitlines() if ln.strip()]


class Controls(TimingCase):
    def test_clean_run_exits_zero(self):
        proc = self.harness().run()
        self.assertEqual(proc.returncode, 0, proc.stderr)

    def test_blocking_run_still_blocks_with_same_rc(self):
        baseline = self.harness(exits={BLOCKER: 1}).run()
        self.assertNotEqual(baseline.returncode, 0, baseline.stderr)
        self.assertEqual(baseline.returncode, 1, baseline.stderr)
        again = self.harness(exits={BLOCKER: 1}).run()
        self.assertEqual(again.returncode, baseline.returncode)

    def test_unwritable_log_location_does_not_change_clean_rc(self):
        h = self.harness()
        self.log_path(h).mkdir()  # a directory where the log file would go
        self.assertEqual(h.run().returncode, 0)


class TimingLog(TimingCase):
    def test_one_json_line_per_run_with_phases_and_rc(self):
        h = self.harness()
        self.assertEqual(h.run().returncode, 0)
        h.run()
        lines = self.read_lines(h)
        self.assertEqual(len(lines), 2, "exactly one JSON line per run")
        rec = lines[0]
        self.assertEqual(rec["exit"], 0)
        self.assertIsInstance(rec["total"], (int, float))
        phases = {p["phase"]: p for p in rec["phases"]}
        for scanner in EXPECTED_SCANNERS:
            self.assertIn(LABELS[scanner], phases, "no phase for %s" % scanner)
            self.assertIsInstance(phases[LABELS[scanner]]["secs"], (int, float))

    def test_blocking_run_logs_its_nonzero_rc(self):
        h = self.harness(exits={BLOCKER: 1})
        proc = h.run()
        self.assertEqual(proc.returncode, 1)
        rec = self.read_lines(h)[-1]
        self.assertEqual(rec["exit"], proc.returncode)


class TimingSummary(TimingCase):
    def test_stderr_summary_has_total_and_exit(self):
        proc = self.harness().run()
        m = re.search(r"pre-commit timing: .*total \S+ \(exit 0\)", proc.stderr)
        self.assertIsNotNone(m, "no `pre-commit timing: ... total ... (exit 0)` in:\n" + proc.stderr)

    def test_stderr_summary_reports_blocking_rc(self):
        proc = self.harness(exits={BLOCKER: 1}).run()
        self.assertIn("(exit %d)" % proc.returncode, proc.stderr)
        self.assertIn("pre-commit timing:", proc.stderr)


class TimingNeverChangesVerdict(TimingCase):
    def _active_and_same_rc(self, code, want_rc):
        h = self.harness(exits={BLOCKER: code})
        proc = h.run()
        self.assertEqual(proc.returncode, want_rc, proc.stderr)
        self.assertEqual(self.read_lines(h)[-1]["exit"], proc.returncode)

    def _unwritable(self, code):
        expected = self.harness(exits={BLOCKER: code}).run().returncode
        h = self.harness(exits={BLOCKER: code})
        self.log_path(h).mkdir()  # append to a directory fails
        proc = h.run()
        self.assertEqual(proc.returncode, expected, proc.stderr)
        self.assertRegex(
            proc.stderr,
            r"pre-commit timing: could not append to .*" + re.escape(LOG_NAME),
        )

    # Timing must be ACTIVE (log written) AND rc unchanged for every outcome
    # class: clean (0), finding (1), undetermined (2).
    def test_active_and_rc_unchanged_clean(self):
        self._active_and_same_rc(0, 0)

    def test_active_and_rc_unchanged_finding(self):
        self._active_and_same_rc(1, 1)

    def test_active_and_rc_unchanged_undetermined(self):
        self._active_and_same_rc(2, 1)

    def test_unwritable_log_warns_and_keeps_rc_clean(self):
        self._unwritable(0)

    def test_unwritable_log_warns_and_keeps_rc_blocking(self):
        self._unwritable(1)


if __name__ == "__main__":
    raise SystemExit(pytest.main([__file__]))
