#!/usr/bin/env python3
"""Unit tests for scripts/check-bench-regression.py.

Stdlib-only (`unittest`), no network. Exercises the three required cases against
tempdir JSONL fixtures, plus the pure helper directly:
  1. no-regression: latest >= baseline (or drop <= threshold) -> exit 0.
  2. regression-exceeds-threshold: drop > threshold -> exit 1.
  3. missing/absent baseline: single run or empty dashboard -> handled without a
     crash, exit 0.
The committed CI fixture (scripts/fixtures/dashboard_sample.jsonl) is also run
end-to-end so the workflow's exact invocation is covered.
"""

from __future__ import annotations

import importlib.util
import json
import tempfile
import unittest
from pathlib import Path

_HERE = Path(__file__).resolve().parent
_SPEC = importlib.util.spec_from_file_location(
    "check_bench_regression", _HERE / "check-bench-regression.py"
)
cbr = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(cbr)


def _write_jsonl(path: Path, rates: list[float]) -> None:
    """Write one RunRecord line per rate, mirroring dashboard.rs field names."""
    with open(path, "w", encoding="utf-8") as f:
        for i, rate in enumerate(rates):
            rec = {
                "timestamp": 1700000000 + i * 86400,
                "resolution_rate": rate,
                "instances": [{"instance_id": f"repo__x-{i}", "resolved": rate >= 0.5}],
                "model": "opus",
                "cost": 1.0,
            }
            f.write(json.dumps(rec) + "\n")


class PureHelper(unittest.TestCase):
    def test_drop_over_threshold_regresses(self):
        self.assertTrue(cbr.resolution_rate_regressed(0.40, 0.50, 0.05))

    def test_drop_within_threshold_ok(self):
        self.assertFalse(cbr.resolution_rate_regressed(0.46, 0.50, 0.05))

    def test_improvement_never_regresses(self):
        self.assertFalse(cbr.resolution_rate_regressed(0.60, 0.50, 0.05))

    def test_drop_exactly_at_threshold_ok(self):
        # `> threshold`, so an exactly-equal drop is allowed (not a regression).
        self.assertFalse(cbr.resolution_rate_regressed(0.45, 0.50, 0.05))


class MainExitCodes(unittest.TestCase):
    def test_no_regression_exit_0(self):
        with tempfile.TemporaryDirectory() as d:
            p = Path(d) / "runs.jsonl"
            _write_jsonl(p, [0.50, 0.52])  # latest improved
            rc = cbr.main(["--dashboard", str(p), "--threshold", "0.05"])
            self.assertEqual(rc, 0)

    def test_small_drop_within_threshold_exit_0(self):
        with tempfile.TemporaryDirectory() as d:
            p = Path(d) / "runs.jsonl"
            _write_jsonl(p, [0.50, 0.47])  # 0.03 drop <= 0.05
            rc = cbr.main(["--dashboard", str(p), "--threshold", "0.05"])
            self.assertEqual(rc, 0)

    def test_regression_over_threshold_exit_1(self):
        with tempfile.TemporaryDirectory() as d:
            p = Path(d) / "runs.jsonl"
            _write_jsonl(p, [0.50, 0.40])  # 0.10 drop > 0.05
            rc = cbr.main(["--dashboard", str(p), "--threshold", "0.05"])
            self.assertEqual(rc, 1)

    def test_single_run_absent_baseline_exit_0(self):
        with tempfile.TemporaryDirectory() as d:
            p = Path(d) / "runs.jsonl"
            _write_jsonl(p, [0.40])  # only one run -> nothing to compare
            rc = cbr.main(["--dashboard", str(p), "--threshold", "0.05"])
            self.assertEqual(rc, 0)

    def test_empty_dashboard_exit_0(self):
        with tempfile.TemporaryDirectory() as d:
            p = Path(d) / "runs.jsonl"
            p.write_text("", encoding="utf-8")
            rc = cbr.main(["--dashboard", str(p), "--threshold", "0.05"])
            self.assertEqual(rc, 0)

    def test_missing_dashboard_file_exit_0(self):
        with tempfile.TemporaryDirectory() as d:
            p = Path(d) / "does-not-exist.jsonl"
            rc = cbr.main(["--dashboard", str(p), "--threshold", "0.05"])
            self.assertEqual(rc, 0)

    def test_explicit_index_baseline(self):
        with tempfile.TemporaryDirectory() as d:
            p = Path(d) / "runs.jsonl"
            _write_jsonl(p, [0.60, 0.55, 0.40])
            # Baseline pinned to the FIRST run (0.60) -> 0.20 drop -> regression.
            rc = cbr.main(
                ["--dashboard", str(p), "--baseline", "0", "--threshold", "0.05"]
            )
            self.assertEqual(rc, 1)

    def test_separate_baseline_file(self):
        with tempfile.TemporaryDirectory() as d:
            dash = Path(d) / "runs.jsonl"
            base = Path(d) / "baseline.jsonl"
            _write_jsonl(dash, [0.40])  # latest
            _write_jsonl(base, [0.50])  # baseline file's last run
            rc = cbr.main(
                ["--dashboard", str(dash), "--baseline", str(base), "--threshold", "0.05"]
            )
            self.assertEqual(rc, 1)


class CommittedFixture(unittest.TestCase):
    def test_committed_fixture_runs_clean(self):
        fixture = _HERE / "fixtures" / "dashboard_sample.jsonl"
        self.assertTrue(fixture.is_file(), f"missing CI fixture: {fixture}")
        # The sample's latest run improved over its predecessor -> exit 0, the
        # exact invocation the workflow performs.
        rc = cbr.main(["--dashboard", str(fixture), "--threshold", "0.05"])
        self.assertEqual(rc, 0)


if __name__ == "__main__":
    unittest.main(verbosity=2)
