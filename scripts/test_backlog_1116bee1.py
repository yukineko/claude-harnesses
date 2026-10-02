#!/usr/bin/env python3
"""Repro for backlog 1116bee1: the propguard suite's mutation kill-rate on
crates/propguard/src/config.rs is below the 0.80 pilot bar (ticket: 48.7%,
20 of 39 survived; re-measured 65.0%, 14 of 40, at e70d48dd).

The measurement IS the test: run the repo's own mutation gate
(scripts/mutation-gate.sh, which shells cargo-mutants and scores outcomes.json
with the deterministic mutategate crate) on exactly the file the ticket names,
and require the gate to pass. Any non-zero gate exit fails this test -- a
below-threshold rate (exit 1) and an unmeasurable run (exit 2, fail-closed) are
both NOT a pass (CLAUDE.md section 3).

Slow (minutes): opt in with RUN_MUTATION_REPROS=1. Without it the test is
SKIPPED, which unittest reports as skipped, never as passed.
"""
import os
import subprocess
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
PILOT = "propguard"
FILE = "crates/propguard/src/config.rs"


class KillRate(unittest.TestCase):
    """Runs the gate ONCE; two tests read the one result so an unmeasurable
    run (exit 2) can never be absorbed by the expected-failure marker."""

    result = None

    @classmethod
    def setUpClass(cls):
        if os.environ.get("RUN_MUTATION_REPROS") != "1":
            raise unittest.SkipTest("slow mutation run; set RUN_MUTATION_REPROS=1")
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

    @unittest.expectedFailure  # backlog 1116bee1: open defect, remove when fixed
    def test_kill_rate_meets_pilot_threshold(self):
        rc, tail = self.result
        self.assertEqual(rc, 0, f"kill-rate below threshold for {FILE} (exit {rc}):\n{tail}")


if __name__ == "__main__":
    unittest.main()
