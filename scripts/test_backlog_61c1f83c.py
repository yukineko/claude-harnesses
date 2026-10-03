#!/usr/bin/env python3
"""Repro for backlog 61c1f83c: no gate compares a plugin's user-facing
descriptions -- crates/<c>/.claude-plugin/plugin.json `description` vs the
`.claude-plugin/marketplace.json` entry for the same plugin -- so a corrected
claim in one copy and a stale overclaim in the other both pass (tdd shipped
such a stale marketplace description for 3 versions; only a tdd-specific
frozen test, crates/tdd/tests/marketplace_description_claims.rs, exists).

The manifest gate is scripts/validate-manifests.py (it already cross-checks
the NAME between the two manifests). This test plants a fixture repo whose two
descriptions disagree -- one scoped, one an unscoped absence claim -- and
requires the gate to reject it. A second test measures the live repo.
"""
from __future__ import annotations

import importlib.util
import io
import json
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent


def load_vm():
    spec = importlib.util.spec_from_file_location("validate_manifests_61c1", HERE / "validate-manifests.py")
    vm = importlib.util.module_from_spec(spec)
    # Compiled from the SOURCE TEXT, deliberately not via the spec loader's
    # exec_module: SourceFileLoader reuses a __pycache__ .pyc validated only by
    # (source mtime at 1 s granularity, size), so a same-second size-preserving
    # edit (e.g. a reordering mutant) would run stale bytecode -- a false GREEN /
    # false mutation SURVIVOR, never a false red. Backlog 05726f9f; do not
    # "simplify" this back. get_source() reads the .py, never the cache;
    # dont_inherit keeps this file's __future__ flags off the subject.
    exec(  # noqa: S102
        compile(spec.loader.get_source(spec.name), spec.origin, "exec", dont_inherit=True),
        vm.__dict__,
    )
    return vm


def fixture(root: Path, plugin_desc: str, market_desc: str) -> None:
    crate = root / "crates" / "demo"
    (crate / ".claude-plugin").mkdir(parents=True)
    (crate / ".claude-plugin" / "plugin.json").write_text(json.dumps(
        {"name": "demo", "version": "0.1.0", "description": plugin_desc}))
    (root / ".claude-plugin").mkdir()
    (root / ".claude-plugin" / "marketplace.json").write_text(json.dumps({
        "name": "m", "owner": {"name": "o"},
        "plugins": [{"name": "demo", "version": "0.1.0", "description": market_desc,
                     "source": {"source": "git-subdir", "url": "u", "path": "crates/demo", "ref": "main"}}],
    }))


def run_gate(root: Path) -> tuple[int, str]:
    vm = load_vm()
    vm.REPO = root
    vm.MARKETPLACE = root / ".claude-plugin" / "marketplace.json"
    out, err = io.StringIO(), io.StringIO()
    with redirect_stdout(out), redirect_stderr(err):
        rc = vm.main()
    return rc, out.getvalue() + err.getvalue()


SCOPED = "Stop hook that blocks when no test is visible in the uncommitted changes."
UNSCOPED = "Stop hook that blocks the turn when implementation code lands without an accompanying test."


class DescriptionGate(unittest.TestCase):
    def test_control_fixture_is_otherwise_valid(self):
        """Anti-vacuity: identical descriptions pass, so a rejection below is
        about the descriptions and nothing else in the fixture."""
        with tempfile.TemporaryDirectory() as t:
            fixture(Path(t), SCOPED, SCOPED)
            rc, out = run_gate(Path(t))
            self.assertEqual(rc, 0, out)

    @unittest.expectedFailure  # backlog 61c1f83c: open defect, remove when fixed
    def test_gate_rejects_disagreeing_descriptions(self):
        with tempfile.TemporaryDirectory() as t:
            fixture(Path(t), SCOPED, UNSCOPED)
            rc, out = run_gate(Path(t))
            self.assertNotEqual(rc, 0, "plugin.json and marketplace.json descriptions disagree "
                                       f"(one carries an unscoped absence claim) yet the gate passed:\n{out}")

    @unittest.expectedFailure  # backlog 61c1f83c: open defect, remove when fixed
    def test_live_repo_descriptions_agree(self):
        market = json.loads((REPO / ".claude-plugin/marketplace.json").read_text(encoding="utf-8"))
        by_path = {}
        for m in sorted(REPO.glob("crates/*/.claude-plugin/plugin.json")):
            by_path[str(m.parent.parent.relative_to(REPO))] = json.loads(m.read_text(encoding="utf-8"))
        self.assertGreater(len(by_path), 0)
        diff = [p["name"] for p in market["plugins"]
                if p["source"]["path"] in by_path
                and by_path[p["source"]["path"]].get("description") != p.get("description")]
        self.assertEqual(diff, [], f"{len(diff)} of {len(market['plugins'])} plugins carry "
                                   f"disagreeing descriptions: {diff}")


if __name__ == "__main__":
    unittest.main()
