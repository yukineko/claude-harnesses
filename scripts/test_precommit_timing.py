#!/usr/bin/env python3
"""RED repro (backlog): .githooks/pre-commit records no timing at all.

.githooks/pre-push times every phase and appends one JSON line per run to
<git-common-dir>/prepush-timing.jsonl (see scripts/test_prepush_timing.py).
pre-commit has no equivalent, so how long a commit spends in its gates cannot
be observed. Measured 2026-11-06 at main b2f2cbd1: grepping .githooks/pre-commit
for time/date/SECONDS/elapsed/duration finds only prose ("Commit time is ..."),
and .git/ holds no precommit-timing.jsonl.

What this pins (observability only — the timing must not change any verdict):
  1. A clean run appends exactly one JSON line to
     <git-common-dir>/precommit-timing.jsonl with `exit` == hook rc (0) and a
     non-empty `phases` list whose entries carry `phase`, `secs` and `rc`
     (the gate's own exit status), one entry per gate label, in run order.
  2. A blocking run (one scanner exits 1) still appends one line, with `exit`
     equal to the (non-zero) hook rc — timing must not swallow the block.
  2b. Per-gate verdicts: on that blocking run the phase of the failing gate has
      rc 1 and every other gate rc 0, so which gate spoke is recoverable from
      the log alone (the input for deciding which gates must run per commit).
  3. stderr carries one `pre-commit timing: ... total Ns (exit RC)` summary.

Run: `python3 scripts/test_precommit_timing.py` (stdlib `unittest` only).
Reuses HookHarness from scripts/test_precommit_hook.py; set
PRECOMMIT_HOOK_UNDER_TEST to point it at another copy of the hook.
"""

from __future__ import annotations

import json
import os
import re
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from test_precommit_hook import (  # noqa: E402
    EXPECTED_SCANNERS,
    HookTestCase,
    _which,
)

_SUMMARY_RE = re.compile(
    r"^pre-commit timing: .*; total (\d+|\?)s \(exit (\d+)\)$", re.MULTILINE
)


class PrecommitTiming(HookTestCase):
    def timed_harness(self, **kw):
        h = self.harness(**kw)
        link = h.bindir / "date"
        if not link.exists():
            os.symlink(_which("date"), link)
        return h, h.root / ".git" / "precommit-timing.jsonl"

    def _lines(self, log):
        self.assertTrue(
            log.exists(), "pre-commit wrote no %s (no timing is recorded)" % log
        )
        return [json.loads(x) for x in log.read_text().splitlines() if x.strip()]

    def test_clean_run_appends_one_timing_line(self):
        h, log = self.timed_harness()
        proc = h.run()
        self.assertEqual(proc.returncode, 0, proc.stderr)
        lines = self._lines(log)
        self.assertEqual(len(lines), 1, lines)
        self.assertEqual(lines[0].get("exit"), 0)
        phases = lines[0].get("phases")
        self.assertTrue(phases, "phases must be non-empty: %r" % lines[0])
        for p in phases:
            self.assertIn("phase", p)
            self.assertIn("secs", p)
            self.assertEqual(p.get("rc"), 0, p)

    def test_blocking_run_still_logs_with_nonzero_exit(self):
        h, log = self.timed_harness(exits={EXPECTED_SCANNERS[0]: 1})
        proc = h.run()
        self.assertNotEqual(proc.returncode, 0, "control: a finding must block")
        lines = self._lines(log)
        self.assertEqual(len(lines), 1, lines)
        self.assertEqual(lines[0].get("exit"), proc.returncode)
        rcs = {p.get("phase"): p.get("rc") for p in lines[0].get("phases", [])}
        self.assertEqual(rcs.get("injectguard"), 1, rcs)
        others = {k: v for k, v in rcs.items() if k != "injectguard"}
        self.assertTrue(others, "other gates must be logged too: %r" % rcs)
        self.assertTrue(all(v == 0 for v in others.values()), rcs)

    def test_stderr_has_one_summary_line(self):
        h, _ = self.timed_harness()
        proc = h.run()
        found = _SUMMARY_RE.findall(proc.stderr)
        self.assertEqual(len(found), 1, proc.stderr)
        self.assertEqual(found[0][1], str(proc.returncode))


if __name__ == "__main__":
    unittest.main()
