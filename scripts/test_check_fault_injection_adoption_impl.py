#!/usr/bin/env python3
"""IMPLEMENTER-WRITTEN tests for scripts/check-fault-injection-adoption.py (backlog 8696dd7e, slice 3).

These were written by the implementer of the script, NOT by an independent agent,
so they carry the author's blind spots (CLAUDE.md 2.(a)). The independent contract
lives in scripts/test_check_fault_injection_adoption.py. Each test below was
observed failing against a deliberate mutation of the script before being kept.

They pin the rulings the independent tests left open:
  * count ABOVE baseline exits 1 (the ratchet tracks the real count exactly)
  * crates/harness-core is excluded from the count (it provides the seam)
  * undetermined exits 2, distinguishable from a regression's exit 1
  * the feature forwarded via [features] or [workspace.dependencies] is "shipped"
  * a crates/<c>/ without Cargo.toml is undetermined unless the root workspace excludes it

Run: python3 scripts/test_check_fault_injection_adoption_impl.py
"""
from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from test_check_fault_injection_adoption import (  # noqa: E402
    ADOPTER,
    NON_ADOPTER,
    Base,
)

CORE_SELF = """[package]
name = "harness-core"
version = "0.1.0"
edition = "2021"

[features]
fault-injection = []

[dev-dependencies]
harness-core = {{ path = ".", features = ["fault-injection"] }}
"""

FORWARDS = NON_ADOPTER + """
[features]
chaos = ["harness-core/fault-injection"]
"""

ROOT_WS = """[workspace]
members = ["crates/*"]
exclude = ["crates/skillonly"]
"""

ROOT_WS_SHIPPED = ROOT_WS + """
[workspace.dependencies]
harness-core = { path = "crates/harness-core", features = ["fault-injection"] }
"""


class AboveBaseline(Base):
    def test_count_above_baseline_exits_1(self):
        r = self.fx({"a": ADOPTER, "b": ADOPTER}, "1\n").run()
        self.assertEqual(r.returncode, 1, r.stderr)
        self.assertIn("baseline", r.stderr)
        self.assertIn("SAME COMMIT", r.stderr)


class HarnessCoreExcluded(Base):
    def test_harness_core_self_dev_dep_is_not_counted(self):
        # harness-core enables the feature on its own dev self-dependency; with
        # no consumer the count must be 0, so baseline 0 passes ...
        self.assertZero(self.fx({"harness-core": CORE_SELF, "a": NON_ADOPTER}, "0\n").run())

    def test_harness_core_cannot_satisfy_a_baseline_of_one(self):
        # ... and it must not stand in for a real adopter.
        r = self.fx({"harness-core": CORE_SELF, "a": NON_ADOPTER}, "1\n").run()
        self.assertEqual(r.returncode, 1, r.stderr)


class UndeterminedIsExit2(Base):
    def test_unparseable_manifest_is_exit_2(self):
        r = self.fx({"a": ADOPTER, "b": "[package\nname = \n"}, "1\n").run()
        self.assertEqual(r.returncode, 2, r.stderr)
        self.assertIn("UNDETERMINED", r.stderr)

    def test_bad_baseline_is_exit_2(self):
        self.assertEqual(self.fx({"a": ADOPTER}, "two\n").run().returncode, 2)

    # The independent suite's missing/empty/non-integer baseline tests use an
    # ADOPTER fixture (count 1), so a script that silently read a bad baseline
    # as 0 still exits non-zero there via the above-baseline rule (observed by
    # mutation). With zero adopters, "bad baseline read as 0" would PASS, so
    # these pin the fail-closed path directly.
    def test_missing_baseline_zero_adopters_is_exit_2(self):
        self.assertEqual(self.fx({"a": NON_ADOPTER}, None).run().returncode, 2)

    def test_empty_baseline_zero_adopters_is_exit_2(self):
        self.assertEqual(self.fx({"a": NON_ADOPTER}, "").run().returncode, 2)

    def test_non_integer_baseline_zero_adopters_is_exit_2(self):
        self.assertEqual(self.fx({"a": NON_ADOPTER}, "zero\n").run().returncode, 2)

    def test_negative_baseline_is_exit_2(self):
        self.assertEqual(self.fx({"a": NON_ADOPTER}, "-1\n").run().returncode, 2)

    def test_regression_is_exit_1_not_2(self):
        self.assertEqual(self.fx({"a": NON_ADOPTER}, "1\n").run().returncode, 1)

    def test_empty_crates_dir_is_exit_2(self):
        f = self.fx({}, "0\n")
        (f.tmp / "crates").mkdir(exist_ok=True)
        self.assertEqual(f.run().returncode, 2)

    def test_crate_dir_without_manifest_is_exit_2(self):
        f = self.fx({"a": NON_ADOPTER}, "0\n")
        (f.tmp / "crates" / "skillonly").mkdir()
        self.assertEqual(f.run().returncode, 2)

    def test_crate_dir_without_manifest_excluded_by_workspace_passes(self):
        f = self.fx({"a": NON_ADOPTER}, "0\n")
        (f.tmp / "crates" / "skillonly").mkdir()
        (f.tmp / "Cargo.toml").write_text(ROOT_WS)
        self.assertZero(f.run())


class ShippedVariants(Base):
    def test_feature_forwarded_from_crate_features_is_rejected(self):
        r = self.fx({"a": FORWARDS}, "0\n").run()
        self.assertEqual(r.returncode, 1, r.stderr)

    def test_feature_in_workspace_dependencies_is_rejected(self):
        f = self.fx({"a": NON_ADOPTER}, "0\n")
        (f.tmp / "Cargo.toml").write_text(ROOT_WS_SHIPPED)
        self.assertEqual(f.run().returncode, 1)


if __name__ == "__main__":
    unittest.main()
