"""Repros for backlog 864eb3e8 and 94b54358 (docs/gate-taxonomy.md prose vs reality).

864eb3e8: the doc names GitHub Actions workflows (`injectguard.yml`, `fail-open.yml`,
  ...) as the "非バイパスの本ゲート" (the real gate). CLAUDE.md §7 bans GitHub Actions
  outright and there is no `.github/` directory, so the doc points readers at a gate
  that does not exist.
94b54358: the trigger legend says pre-commit is fail-soft (passes when python3 / the
  script is absent), but .githooks/pre-commit blocks a missing scanner.

Each test asserts the CORRECT documentation, so it fails while the defect is open.
Stdlib only, read-only.
"""
import re
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
DOC = REPO / "docs" / "gate-taxonomy.md"


class Backlog864eb3e8(unittest.TestCase):
    def test_control_no_github_dir(self):
        self.assertFalse((REPO / ".github").exists())

    @unittest.expectedFailure  # backlog 864eb3e8: open defect, remove when fixed
    def test_doc_does_not_present_a_gha_workflow_as_a_gate(self):
        offenders = []
        for n, line in enumerate(DOC.read_text(encoding="utf-8").splitlines(), 1):
            if re.search(r"CI（`[\w.-]+\.yml`", line) or "非バイパスの本ゲート" in line:
                offenders.append(f"{n}: {line[:120]}")
        self.assertEqual(offenders, [], "gate-taxonomy.md presents a GitHub Actions workflow as a gate")


class Backlog94b54358(unittest.TestCase):
    def test_control_pre_commit_blocks_a_missing_scanner(self):
        hook = (REPO / ".githooks" / "pre-commit").read_text(encoding="utf-8")
        self.assertIn("is missing — cannot determine, so blocking.", hook)

    @unittest.expectedFailure  # backlog 94b54358: open defect, remove when fixed
    def test_legend_does_not_call_pre_commit_fail_soft_pass_through(self):
        text = DOC.read_text(encoding="utf-8")
        self.assertNotIn("python3/スクリプト不在時は素通り", text)


if __name__ == "__main__":
    unittest.main()
