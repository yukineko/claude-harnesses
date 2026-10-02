#!/usr/bin/env python3
"""Closure regression for backlog 5c6a3b69.

5c6a3b69: `autonomy_invariant::askuserquestion_sites_match_frozen_allowlist`
was red because crates/flow/skills/flow/SKILL.md carried 11 `AskUserQuestion`
lines against an allowlist of 10. The ticket required the NEW site to be
re-audited against the two-stop invariant BEFORE the allowlist moved (raising
the number to silence the test was forbidden).

Pinned here, independently of the Rust test:
  1. the flow SKILL's AskUserQuestion line count (same rule as the Rust test:
     every line that mentions it) equals the ASK_ALLOWLIST entry for flow;
  2. the allowlist rationale carries a written audit of the 10 -> 11 site
     introduced by 5289312f (the re-audit the ticket asked for).
"""

from __future__ import annotations

import os
import re
import unittest

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SKILL = os.path.join(REPO, "crates", "flow", "skills", "flow", "SKILL.md")
INVARIANT = os.path.join(REPO, "crates", "condukt", "tests", "autonomy_invariant.rs")


def _skill_count() -> int:
    with open(SKILL, encoding="utf-8") as f:
        return sum(1 for line in f if "AskUserQuestion" in line)


def _allowlist_text() -> str:
    with open(INVARIANT, encoding="utf-8") as f:
        return f.read()


def _allowlisted_flow() -> int:
    m = re.search(r'\("flow/skills/flow/SKILL\.md",\s*(\d+)\)', _allowlist_text())
    if m is None:
        raise AssertionError("no flow entry in ASK_ALLOWLIST")
    return int(m.group(1))


class Backlog5c6a3b69(unittest.TestCase):
    def test_flow_skill_count_matches_allowlist(self):
        self.assertEqual(_skill_count(), _allowlisted_flow())

    def test_the_10_to_11_site_has_a_written_audit(self):
        text = _allowlist_text()
        self.assertIn("The 11th is", text)
        self.assertIn("5289312f", text)


if __name__ == "__main__":
    unittest.main()
