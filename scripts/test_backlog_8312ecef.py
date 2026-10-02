#!/usr/bin/env python3
"""Repro for backlog 8312ecef.

check-doc-claims.py and check-claudemd-claims.py judge the git INDEX by default
(`--source index`), but the .githooks/pre-commit header rows that describe
these two gates say only WHAT they catch, not WHICH TREE they judge.

Property asserted: the header table row of each of `doc-claims` and
`claudemd-claims` names the tree it judges (the word "index"), and the scripts
really do default to the index (precondition, so the assertion is about a true
fact).

Written by an independent auditor, not an implementer.
"""

from __future__ import annotations

import os
import re
import subprocess
import sys
import unittest

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
HOOK = os.path.join(REPO, ".githooks", "pre-commit")


def _header_row(gate: str) -> str:
    """The `#   <gate>   <script>   ...` row of the header table, with its
    continuation lines, up to the next row or the end of the comment block."""
    with open(HOOK, encoding="utf-8") as f:
        lines = f.read().splitlines()
    row_re = re.compile(r"^#   (\S+)\s{2,}\S+\.py\s")
    out: list[str] = []
    inside = False
    for line in lines:
        m = row_re.match(line)
        if m:
            if inside:
                break
            inside = m.group(1) == gate
        elif inside and not line.startswith("#   "):
            break
        if inside:
            out.append(line)
    if not out:
        raise AssertionError(f"no header row for {gate!r} in .githooks/pre-commit")
    return "\n".join(out)


class PreCommitHeaderNamesTree(unittest.TestCase):
    def test_precondition_scripts_default_to_index(self) -> None:
        for script in ("check-doc-claims.py", "check-claudemd-claims.py"):
            p = subprocess.run(
                [sys.executable, os.path.join(REPO, "scripts", script), "--help"],
                capture_output=True,
                text=True,
            )
            self.assertEqual(p.returncode, 0, p.stderr)
            self.assertRegex(p.stdout, r"index \(default\)", script)

    @unittest.expectedFailure  # backlog 8312ecef: open defect
    def test_header_rows_say_which_tree_is_judged(self) -> None:
        missing = {
            g: _header_row(g)
            for g in ("doc-claims", "claudemd-claims")
            if "index" not in _header_row(g).lower()
        }
        self.assertEqual(missing, {}, f"header rows that do not name the judged tree: {missing}")


if __name__ == "__main__":
    unittest.main()
