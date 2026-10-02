#!/usr/bin/env python3
"""Repro for backlog 49a457c8: crates/tdd/README.md and the tdd SKILL.md still
carry the UNSCOPED "implementation cannot land without a test" claim, while the
gate only inspects the UNCOMMITTED diff (crates/tdd/src/git.rs: git status
--porcelain, git diff -U0, git diff --cached -U0, git ls-files --others; the
commit log is never read). marketplace.json / plugin.json / Cargo.toml were
corrected (b61d68b4, f70706e6); these three distribution surfaces were not.

Each surface is read fail-closed: a missing file is an error, never "the claim
is absent" (CLAUDE.md section 3).
"""
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
README = REPO / "crates/tdd/README.md"
SKILL = REPO / "crates/tdd/skills/tdd/SKILL.md"


def read(p: Path) -> str:
    return p.read_text(encoding="utf-8")  # raises if absent: undetermined != clean


def offending(text: str, needle: str, scope_words) -> list:
    """Lines carrying `needle` with no uncommitted-scope qualifier."""
    out = []
    for i, line in enumerate(text.splitlines(), 1):
        if needle in line and not any(w in line for w in scope_words):
            out.append(f"{i}: {line.strip()}")
    return out


SCOPE_EN = ("uncommitted", "visible in")
SCOPE_JA = ("未コミット", "uncommitted")


class TddUnscopedClaims(unittest.TestCase):
    def test_control_files_read_and_describe_the_gate(self):
        """Anti-vacuity: both surfaces exist and still describe `tdd gate`."""
        self.assertIn("tdd gate", read(README))
        self.assertIn("Stop hook", read(SKILL))

    @unittest.expectedFailure  # backlog 49a457c8: open defect, remove when fixed
    def test_readme_does_not_claim_code_cannot_land_without_a_test(self):
        bad = offending(read(README), "without a test", SCOPE_EN)
        self.assertEqual(bad, [], "README.md states an unscoped absence claim:\n" + "\n".join(bad))

    @unittest.expectedFailure  # backlog 49a457c8: open defect, remove when fixed
    def test_skill_does_not_claim_test_less_implementation_is_physically_stopped(self):
        text = read(SKILL)
        bad = offending(text, "テスト無しの実装を物理的に止める", SCOPE_JA)
        bad += offending(text, "テスト無し実装のブロック", SCOPE_JA)
        self.assertEqual(bad, [], "SKILL.md states an unscoped absence claim:\n" + "\n".join(bad))


if __name__ == "__main__":
    unittest.main()
