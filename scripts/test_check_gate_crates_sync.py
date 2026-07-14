#!/usr/bin/env python3
"""Unit tests for scripts/check-gate-crates-sync.py.

Stdlib-only (`unittest`), no network. Exercises:
  1. the real repo state (all sources satisfy their required relation) -> exit 0.
  2. the relation-based design: canonical/exact/superset/mirror, including the
     "audit-only" crate (e.g. backlog) that continuous-audit.sh and the SKILL.md
     doc may carry beyond the canonical GATE_CRATES set.
  3. synthetic drift fixtures (one source violates its relation) -> exit 1, detected.
"""
import importlib.util
import os
import tempfile
import unittest
from pathlib import Path

_HERE = Path(__file__).resolve().parent
_SPEC = importlib.util.spec_from_file_location(
    "check_gate_crates_sync", _HERE / "check-gate-crates-sync.py"
)
cgcs = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(cgcs)

REPO_ROOT = _HERE.parent

CANONICAL = "blastguard propguard specguard stuckguard mutategate overwatch"
CANONICAL_SET = set(CANONICAL.split())


def _write(path, text):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")


def _make_fixture_repo(
    tmp,
    *,
    pre_push_extra=(),
    pre_push_missing=(),
    ca_extra=(),
    ca_missing=(),
    skill_extra=(),
    skill_missing=(),
):
    """Build a tmp dir with all 4 sources. By default everything is exactly in
    sync with CANONICAL. The `*_extra`/`*_missing` args perturb one source's
    crate set relative to CANONICAL (for continuous-audit.sh/SKILL.md) so
    callers can exercise the superset/mirror relations, not just exact match."""

    def apply(base, extra, missing):
        return (set(base) | set(extra)) - set(missing)

    pre_push_set = apply(CANONICAL_SET, pre_push_extra, pre_push_missing)
    ca_set = apply(CANONICAL_SET, ca_extra, ca_missing)
    skill_set = apply(CANONICAL_SET, skill_extra, skill_missing)

    _write(tmp / "scripts" / "rollout-plugins.sh", f'#!/bin/sh\nGATE_CRATES="{CANONICAL}"\n')
    _write(
        tmp / "scripts" / "continuous-audit.sh",
        f'#!/bin/sh\nDEFAULT_TARGETS="{",".join(sorted(ca_set))}"\n',
    )
    _write(
        tmp / ".githooks" / "pre-push",
        f"#!/bin/sh\nGATE_PATTERN='^crates/({'|'.join(sorted(pre_push_set))})/'\n",
    )
    _write(
        tmp / "crates" / "overwatch" / "skills" / "continuous-audit" / "SKILL.md",
        "## 対象 crate (既定)\n\n"
        f"既定の target は fleet の **GATE crates**: `{','.join(sorted(skill_set))}`\n"
        "(同期の説明文)。`--target` で上書きできる。\n",
    )
    return tmp


def _by_path(parsed):
    return {rel_path: crates for rel_path, _mode, crates in parsed}


class RealRepoState(unittest.TestCase):
    def test_current_repo_is_in_sync(self):
        ok, canonical, parsed = cgcs.check(repo=str(REPO_ROOT))
        self.assertTrue(
            ok, f"repo GATE_CRATES sources drifted: canonical={canonical} parsed={parsed}"
        )

    def test_main_exits_zero_against_real_repo(self):
        cwd = os.getcwd()
        os.chdir(REPO_ROOT)
        try:
            rc = cgcs.main()
        finally:
            os.chdir(cwd)
        self.assertEqual(rc, 0)


class DriftDetection(unittest.TestCase):
    def test_fully_synced_fixture_passes(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo = _make_fixture_repo(Path(tmp))
            ok, canonical, _ = cgcs.check(repo=str(repo))
            self.assertTrue(ok)
            self.assertEqual(canonical, CANONICAL_SET)

    def test_audit_only_addition_in_superset_and_mirror_passes(self):
        """continuous-audit.sh and SKILL.md both carry `backlog` beyond the
        canonical GATE_CRATES set (superset + mirror satisfied); pre-push
        stays GATE-crates-only (exact). This must PASS, not drift."""
        with tempfile.TemporaryDirectory() as tmp:
            repo = _make_fixture_repo(
                Path(tmp), ca_extra=("backlog",), skill_extra=("backlog",)
            )
            ok, canonical, parsed = cgcs.check(repo=str(repo))
            self.assertTrue(ok, f"expected audit-only addition to pass: {parsed}")
            by_path = _by_path(parsed)
            self.assertIn("backlog", by_path["scripts/continuous-audit.sh"])
            self.assertNotIn("backlog", by_path[".githooks/pre-push"])

    def test_pre_push_missing_a_crate_is_detected(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo = _make_fixture_repo(Path(tmp), pre_push_missing=("overwatch",))
            ok, canonical, parsed = cgcs.check(repo=str(repo))
            self.assertFalse(ok)
            by_path = _by_path(parsed)
            self.assertNotEqual(by_path[".githooks/pre-push"], canonical)
            self.assertNotIn("overwatch", by_path[".githooks/pre-push"])

    def test_continuous_audit_missing_a_canonical_crate_is_detected(self):
        """DEFAULT_TARGETS must be a superset of canonical; dropping a GATE
        crate violates the superset relation even with no extra crates."""
        with tempfile.TemporaryDirectory() as tmp:
            repo = _make_fixture_repo(Path(tmp), ca_missing=("overwatch",))
            ok, canonical, parsed = cgcs.check(repo=str(repo))
            self.assertFalse(ok)
            by_path = _by_path(parsed)
            self.assertFalse(canonical <= by_path["scripts/continuous-audit.sh"])

    def test_skill_md_diverging_from_continuous_audit_is_detected(self):
        """SKILL.md must mirror continuous-audit.sh's DEFAULT_TARGETS exactly.
        continuous-audit.sh gains `backlog` but SKILL.md doesn't -> drift."""
        with tempfile.TemporaryDirectory() as tmp:
            repo = _make_fixture_repo(Path(tmp), ca_extra=("backlog",))
            ok, canonical, parsed = cgcs.check(repo=str(repo))
            self.assertFalse(ok)
            by_path = _by_path(parsed)
            self.assertNotEqual(
                by_path["crates/overwatch/skills/continuous-audit/SKILL.md"],
                by_path["scripts/continuous-audit.sh"],
            )

    def test_main_exits_one_on_drifted_fixture(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo = _make_fixture_repo(Path(tmp), pre_push_missing=("overwatch",))
            cwd = os.getcwd()
            os.chdir(repo)
            try:
                rc = cgcs.main()
            finally:
                os.chdir(cwd)
            self.assertEqual(rc, 1)


if __name__ == "__main__":
    unittest.main()
