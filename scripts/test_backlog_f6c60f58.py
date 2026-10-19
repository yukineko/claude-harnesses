#!/usr/bin/env python3
"""Repro for backlog f6c60f58: the stuckguard suite's mutation kill-rate on
crates/stuckguard/src/detect.rs is below the 0.80 pilot bar (ticket: 78.8%,
11 of 52 viable mutants survived).

The measurement IS the test: run the repo's own mutation gate
(scripts/mutation-gate.sh, which shells cargo-mutants and scores outcomes.json
with the deterministic mutategate crate) on exactly the file the ticket names,
and require the gate to pass. Any non-zero gate exit fails this test -- a
below-threshold rate (exit 1) and an unmeasurable run (exit 2, fail-closed) are
both NOT a pass (CLAUDE.md section 3).

Slow (minutes): the measurement always runs. An opt-in skip made the suite
report `Ran 0 tests`, which check-workspace-tests.py rightly reports as
undetermined (backlog 69e34456).

The detect.rs:128:47 mutant that used to flip between caught and missed was
"caught" only when the racing anchor test (backlog 07bfdca0) happened to fail.
That race is fixed and a dedicated detect.rs test now kills the mutant, so the
threshold test carries no expectedFailure marker.
"""
import os
import subprocess
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
PILOT = "stuckguard"
FILE = "crates/stuckguard/src/detect.rs"


class KillRate(unittest.TestCase):
    """Runs the gate ONCE; two tests read the one result so an unmeasurable
    run (exit 2) can never be absorbed by the expected-failure marker."""

    result = None

    @classmethod
    def setUpClass(cls):
        # fixture precondition: cargo-mutants does not create target/ itself
        (REPO / "target").mkdir(exist_ok=True)
        env = dict(os.environ, PILOT=PILOT, MUTANTS_EXTRA=f"--file {FILE}")
        r = subprocess.run(["bash", str(REPO / "scripts" / "mutation-gate.sh")],
                           cwd=REPO, env=env, capture_output=True, text=True)
        cls.result = (r.returncode, (r.stdout + r.stderr)[-3000:])
        print(cls.result[1])

    def test_measurement_completed(self):
        """Not RED-marked: exit 2 (no trustworthy result set) is a broken
        measurement, never evidence about the kill-rate."""
        rc, tail = self.result
        self.assertIn(rc, (0, 1), f"mutation gate could not measure (exit {rc}):\n{tail}")

    def test_kill_rate_meets_pilot_threshold(self):
        rc, tail = self.result
        self.assertEqual(rc, 0, f"kill-rate below threshold for {FILE} (exit {rc}):\n{tail}")


if __name__ == "__main__":
    unittest.main()
