#!/usr/bin/env python3
"""Repro for backlog 5a746cb9: crates/tdd/skills/tdd/SKILL.md tells the agent
that a Stop block means "there is implementation with no test attached"
(テストが付いていない実装がある). That is the diagnosis the gate's own frozen
test forbids the block reason from making (crates/tdd/src/gate.rs:
`!lower.contains("no accompanying test")`, "tdd only inspected the UNCOMMITTED
diff; it cannot assert the change has no accompanying test"). The honest
reading: no test was VISIBLE in the uncommitted changes.

Reads are fail-closed (a missing file errors; it never reads as "absent").
"""
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
SKILL = REPO / "crates/tdd/skills/tdd/SKILL.md"
GATE = REPO / "crates/tdd/src/gate.rs"

CLAIM = "テストが付いていない実装がある"
SCOPE = ("未コミット", "uncommitted", "見えない", "visible")


class SkillDiagnosis(unittest.TestCase):
    def test_control_gate_forbids_the_unscoped_diagnosis(self):
        """Anti-vacuity: the gate's own frozen test still forbids the claim, so
        the contradiction this file pins is real at this rev."""
        self.assertIn('no accompanying test', GATE.read_text(encoding="utf-8"))
        self.assertIn("Stop でブロックされる", SKILL.read_text(encoding="utf-8"))

    @unittest.expectedFailure  # backlog 5a746cb9: open defect, remove when fixed
    def test_skill_does_not_diagnose_a_block_as_test_less_implementation(self):
        bad = [f"{i}: {l.strip()}"
               for i, l in enumerate(SKILL.read_text(encoding="utf-8").splitlines(), 1)
               if CLAIM in l and not any(w in l for w in SCOPE)]
        self.assertEqual(bad, [], "SKILL.md re-narrates the block as an unscoped absence claim:\n"
                         + "\n".join(bad))


if __name__ == "__main__":
    unittest.main()
