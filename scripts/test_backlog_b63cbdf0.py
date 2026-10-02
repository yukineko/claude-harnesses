#!/usr/bin/env python3
"""Repro for backlog b63cbdf0: crates/autoflow/README.ja.md still states a
count-based safety cap ("自動は 4 回まで、5 回目以降はユーザーに確認") as the
spec, while the code has no such cap: continuation is progress-based
(`stuck_threshold`), and the same README's own line 21 says so. A reader is
told a runaway cap exists that the code does not have (CLAUDE.md section 4).

Control (anti-vacuity): the code really has stuck_threshold and really has no
count cap, so the prose is wrong rather than the code.
"""
import re
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
README_JA = REPO / "crates/autoflow/README.ja.md"
SRC = REPO / "crates/autoflow/src"


class AutoflowCountCapProse(unittest.TestCase):
    def test_control_code_is_progress_based_with_no_count_cap(self):
        code = "\n".join(p.read_text(encoding="utf-8") for p in sorted(SRC.glob("*.rs")))
        self.assertGreater(len(code), 0)
        self.assertIn("stuck_threshold", code)
        self.assertIsNone(re.search(r"max_backlog_prompts|max_auto|auto_cap|MAX_AUTO|max_blocks", code))

    @unittest.expectedFailure  # backlog b63cbdf0: open defect, remove when fixed
    def test_readme_ja_does_not_state_a_count_cap(self):
        t = README_JA.read_text(encoding="utf-8")
        bad = [l.strip()[:160] for l in t.splitlines()
               if re.search(r"4 ?回まで|5 ?回目|自動ブロックには上限", l)]
        self.assertEqual(bad, [], "README.ja.md states a count cap the code lacks:\n" + "\n".join(bad))


if __name__ == "__main__":
    unittest.main()
