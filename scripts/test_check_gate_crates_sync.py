#!/usr/bin/env python3
"""Unit tests for scripts/check-gate-crates-sync.py.

Stdlib-only (`unittest`), no network. Exercises:
  1. the real repo state (all 4 sources agree) -> exit 0.
  2. a synthetic drift fixture (one source missing a crate) -> exit 1, detected.
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


def _write(path, text):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")


def _make_fixture_repo(tmp, *, drop_from="pre_push"):
    """Build a tmp dir with all 4 sources, optionally dropping one crate from
    the given source to simulate drift. `drop_from` in
    {"none", "continuous_audit", "pre_push", "skill_md"}."""
    canonical = "blastguard propguard specguard stuckguard mutategate overwatch"
    ca_targets = "blastguard,propguard,specguard,stuckguard,mutategate,overwatch"
    pre_push_pattern = "blastguard|propguard|specguard|stuckguard|mutategate|overwatch"
    skill_csv = "blastguard,propguard,specguard,stuckguard,mutategate,overwatch"

    if drop_from == "continuous_audit":
        ca_targets = "blastguard,propguard,specguard,stuckguard,mutategate"
    elif drop_from == "pre_push":
        pre_push_pattern = "blastguard|propguard|specguard|stuckguard|mutategate"
    elif drop_from == "skill_md":
        skill_csv = "blastguard,propguard,specguard,stuckguard,mutategate"

    _write(tmp / "scripts" / "rollout-plugins.sh",
           f'#!/bin/sh\nGATE_CRATES="{canonical}"\n')
    _write(tmp / "scripts" / "continuous-audit.sh",
           f'#!/bin/sh\nDEFAULT_TARGETS="{ca_targets}"\n')
    _write(tmp / ".githooks" / "pre-push",
           f"#!/bin/sh\nGATE_PATTERN='^crates/({pre_push_pattern})/'\n")
    _write(
        tmp / "crates" / "overwatch" / "skills" / "continuous-audit" / "SKILL.md",
        "## 対象 crate (既定)\n\n"
        f"既定の target は fleet の **GATE crates**: `{skill_csv}`\n"
        "(同期の説明文)。`--target` で上書きできる。\n",
    )
    return tmp


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
            repo = _make_fixture_repo(Path(tmp), drop_from="none")
            ok, canonical, _ = cgcs.check(repo=str(repo))
            self.assertTrue(ok)
            self.assertEqual(
                canonical,
                {"blastguard", "propguard", "specguard", "stuckguard", "mutategate", "overwatch"},
            )

    def test_pre_push_missing_a_crate_is_detected(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo = _make_fixture_repo(Path(tmp), drop_from="pre_push")
            ok, canonical, parsed = cgcs.check(repo=str(repo))
            self.assertFalse(ok)
            by_path = dict(parsed)
            self.assertNotEqual(by_path[".githooks/pre-push"], canonical)
            self.assertNotIn("overwatch", by_path[".githooks/pre-push"])

    def test_continuous_audit_missing_a_crate_is_detected(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo = _make_fixture_repo(Path(tmp), drop_from="continuous_audit")
            ok, canonical, parsed = cgcs.check(repo=str(repo))
            self.assertFalse(ok)
            by_path = dict(parsed)
            self.assertNotEqual(by_path["scripts/continuous-audit.sh"], canonical)

    def test_skill_md_missing_a_crate_is_detected(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo = _make_fixture_repo(Path(tmp), drop_from="skill_md")
            ok, canonical, parsed = cgcs.check(repo=str(repo))
            self.assertFalse(ok)
            by_path = dict(parsed)
            self.assertNotEqual(
                by_path["crates/overwatch/skills/continuous-audit/SKILL.md"], canonical
            )

    def test_main_exits_one_on_drifted_fixture(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo = _make_fixture_repo(Path(tmp), drop_from="pre_push")
            cwd = os.getcwd()
            os.chdir(repo)
            try:
                rc = cgcs.main()
            finally:
                os.chdir(cwd)
            self.assertEqual(rc, 1)


if __name__ == "__main__":
    unittest.main()
