#!/usr/bin/env python3
"""Repro for backlog 8e348859: ctxrot's SAMPLE_CONFIG (crates/ctxrot/src/main.rs)
tells the user that the Stop nudge "causes Claude Code to continue the session
so Claude itself can run /compact". Slash commands are user-typed; the model
has no tool that runs /compact (documented in this repo's
compaction-not-hook-triggerable note, checked against the Claude Code docs on
2026-06-30). The comment therefore promises a mechanism that does not exist
(CLAUDE.md section 4).

What this test observes: the claim is present in the shipped template text.
What it does NOT observe: the model's inability to run /compact, which is a
property of the Claude Code runtime and not observable from this repo.
"""
import re
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
MAIN = REPO / "crates/ctxrot/src/main.rs"


class CtxrotCompactClaim(unittest.TestCase):
    def test_control_sample_config_present(self):
        self.assertIn("auto_compact_enabled", MAIN.read_text(encoding="utf-8"))

    @unittest.expectedFailure  # backlog 8e348859: open defect, remove when fixed
    def test_sample_config_does_not_claim_claude_can_run_compact(self):
        flat = re.sub(r"\s*\n\s*#\s*", " ", MAIN.read_text(encoding="utf-8"))
        hits = re.findall(r"[^.]*Claude itself can run /compact[^.]*", flat)
        self.assertEqual(hits, [], f"ctxrot main.rs claims the model can run /compact: {hits}")


if __name__ == "__main__":
    unittest.main()
