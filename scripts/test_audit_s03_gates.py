#!/usr/bin/env python3
"""Reproduction tests for open backlog items audited in shard s03-backlog (2026-10-02).

Each test asserts the property the ticket says is MISSING and is marked
expectedFailure: it FAILS on the code as measured (RED observed). Remove the
decorator when the defect is fixed.
"""
import pathlib
import unittest

ROOT = pathlib.Path(__file__).resolve().parent.parent


class AuditS03(unittest.TestCase):
    @unittest.expectedFailure  # backlog ece51b35: open defect
    def test_precommit_family_inspects_backlog_conflict_markers(self):
        """No gate script or hook inspects a conflict-marked .backlog/*.toml."""
        hits = []
        files = list((ROOT / ".githooks").iterdir()) + list((ROOT / "scripts").glob("*.py"))
        for p in files:
            if p.name.startswith("test_") or not p.is_file():
                continue
            t = p.read_text(errors="replace")
            if ".backlog" in t and ("<<<<<<<" in t or "conflict marker" in t.lower()):
                hits.append(p.name)
        self.assertTrue(hits, "no gate inspects .backlog/*.toml for conflict markers")

    @unittest.expectedFailure  # backlog 400d685b: open defect
    def test_prepush_enablement_text_does_not_claim_gate_guards_nothing(self):
        t = (ROOT / ".githooks" / "pre-push").read_text()
        self.assertNotIn("the gate reports nothing and guards nothing", t)

    @unittest.expectedFailure  # backlog 233f819c: open defect
    def test_stop_verify_block_message_names_dirty_paths(self):
        t = (ROOT / "scripts" / "stop-verify-worktree.py").read_text()
        self.assertTrue(
            "porcelain" in t and ("dirty paths" in t.lower() or "paths:" in t.lower()),
            "block message does not name the dirty paths it attributes",
        )


if __name__ == "__main__":
    unittest.main()
