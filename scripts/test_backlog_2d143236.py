#!/usr/bin/env python3
"""Repro for backlog 2d143236.

docs/gate-taxonomy.md names GitHub Actions workflow files (doc-claims.yml,
test-weakening.yml, version-lockstep.yml, ...) as enforcement points, but the
repository has no .github/workflows/ at all (GHA is banned repo-wide, CLAUDE.md
section 7). These are bare filenames, so the doc-claims gate cannot see them.

Property asserted: every `*.yml` name the document mentions exists under
.github/workflows/.

Written by an independent auditor, not an implementer.
"""

from __future__ import annotations

import os
import re
import unittest

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DOC = os.path.join(REPO, "docs", "gate-taxonomy.md")
WORKFLOWS = os.path.join(REPO, ".github", "workflows")


class TaxonomyWorkflowNames(unittest.TestCase):
    @unittest.expectedFailure  # backlog 2d143236: open defect
    def test_named_workflow_files_exist(self) -> None:
        with open(DOC, encoding="utf-8") as f:
            text = f.read()
        names = sorted(set(re.findall(r"[A-Za-z0-9_.-]+\.yml\b", text)))
        missing = [n for n in names if not os.path.isfile(os.path.join(WORKFLOWS, n))]
        self.assertEqual(
            missing,
            [],
            f"gate-taxonomy.md names {len(missing)} workflow file(s) that do not "
            f"exist under .github/workflows/: {missing}",
        )


if __name__ == "__main__":
    unittest.main()
