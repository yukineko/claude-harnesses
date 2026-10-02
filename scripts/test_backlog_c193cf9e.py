#!/usr/bin/env python3
"""Repro for backlog c193cf9e: the starter config templates that `init` writes
into a user project (and their *.example.toml twins) still carry the
pre-b61d68b4 overclaims for tdd, propguard and reviewgate:

  tdd        "blocks the turn when implementation code was ADDED without
             an accompanying test" -- the gate scans the UNCOMMITTED diff only;
             gate.rs's frozen test forbids "no accompanying test" in the reason.
  propguard  "It blocks when fewer than `threshold` properties hold." -- in the
             written default mode = "inject" nothing is counted (gate.rs builds
             Verified { satisfied: 0, findings: None } and blocks on a new diff).
  reviewgate "the running agent reviews its own changes" -- review.rs documents
             why the self-review wording was removed from the block message.

Each test reads the Rust source (the const `init` writes) AND the example
file. Reads are fail-closed.
"""
import re
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent


def text(rel: str) -> str:
    return (REPO / rel).read_text(encoding="utf-8")


def flat(s: str) -> str:
    """Join comment lines so a claim wrapped across `#` lines is still found."""
    return re.sub(r"\s*\n\s*#\s*", " ", s)


class StarterTemplates(unittest.TestCase):
    def test_control_templates_exist_and_default_propguard_to_inject(self):
        self.assertIn("STARTER", text("crates/propguard/src/main.rs"))
        self.assertRegex(text("crates/propguard/propguard.example.toml"), r'mode\s*=\s*"inject"')
        self.assertIn("STARTER_CONFIG", text("crates/tdd/src/main.rs"))

    @unittest.expectedFailure  # backlog c193cf9e: open defect, remove when fixed
    def test_tdd_templates_do_not_claim_added_code_lacks_a_test(self):
        hits = [p for p in ("crates/tdd/src/main.rs", "crates/tdd/tdd.example.toml")
                if "without an accompanying test" in flat(text(p))]
        self.assertEqual(hits, [], f"unscoped 'without an accompanying test' in {hits}")

    @unittest.expectedFailure  # backlog c193cf9e: open defect, remove when fixed
    def test_propguard_templates_do_not_describe_inject_mode_as_a_count(self):
        hits = [p for p in ("crates/propguard/src/main.rs", "crates/propguard/propguard.example.toml")
                if "blocks when fewer than `threshold` properties hold" in flat(text(p))]
        self.assertEqual(hits, [], f"measurement claim in the inject-default template: {hits}")

    @unittest.expectedFailure  # backlog c193cf9e: open defect, remove when fixed
    def test_reviewgate_templates_do_not_claim_agent_reviews_its_own_changes(self):
        hits = [p for p in ("crates/reviewgate/src/main.rs", "crates/reviewgate/reviewgate.example.toml")
                if re.search(r"agent reviews its own\s*(changes|diff)", flat(text(p)))]
        self.assertEqual(hits, [], f"self-review claim in starter template: {hits}")


if __name__ == "__main__":
    unittest.main()
