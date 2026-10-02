#!/usr/bin/env python3
"""Repro for backlog 7e61998a.

docs/gate-taxonomy.md and docs/autoflow-verdict-audit.md carry stale
`path:line` citations that check-doc-claims.py reports only as `exempt`
findings (quote-not-found / line-drifted / path-not-found), so the gate stays
`clean` while the citations no longer describe reality.

Property asserted: check-doc-claims.py, run on each document against the index
(its default source), reports zero findings at all (exempt or not).

Written by an independent auditor, not an implementer.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import unittest

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
CHECK = os.path.join(REPO, "scripts", "check-doc-claims.py")
DOCS = ("docs/gate-taxonomy.md", "docs/autoflow-verdict-audit.md")


def _findings(doc: str) -> list[dict]:
    p = subprocess.run(
        [sys.executable, CHECK, "--repo", REPO, "--doc", doc, "--json"],
        cwd=REPO,
        capture_output=True,
        text=True,
    )
    try:
        data = json.loads(p.stdout)
    except json.JSONDecodeError as e:
        raise AssertionError(
            f"check-doc-claims gave no JSON for {doc} (rc={p.returncode}): {e}; "
            f"stderr={p.stderr!r}"
        )
    findings = data.get("findings")
    if not isinstance(findings, list):
        raise AssertionError(f"no findings list for {doc}: {data!r}")
    return findings


class StaleCitations(unittest.TestCase):
    @unittest.expectedFailure  # backlog 7e61998a: open defect
    def test_docs_have_no_stale_citations(self) -> None:
        stale = {}
        for doc in DOCS:
            f = _findings(doc)
            if f:
                stale[doc] = (len(f), [(x["doc_line"], x["kind"], x["path"]) for x in f[:5]])
        self.assertEqual(stale, {}, f"stale citations (count, first five): {stale}")


if __name__ == "__main__":
    unittest.main()
