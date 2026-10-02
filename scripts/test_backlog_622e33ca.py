#!/usr/bin/env python3
"""Repro for backlog 622e33ca: docs/autoflow-verdict-audit.md lost GFM grid
rendering in several tables when doc-claim-exempt markers were inserted:

  * a marker on its OWN line right after a table's delimiter row starts an
    HTML block, which ends the table -- every body row below it renders as a
    paragraph of raw pipes;
  * a marker appended as an extra cell OF the delimiter row makes that row an
    invalid delimiter row (a delimiter cell is only `:?-+:?`), so the table is
    never recognised at all.

Stdlib-only structural check of exactly those two GFM rules (no renderer
dependency). Content is intact; the defect is the rendering.
"""
import re
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
DOC = REPO / "docs/autoflow-verdict-audit.md"

DELIM_CELL = re.compile(r"^\s*:?-{3,}:?\s*$")


def cells(line: str) -> list:
    s = line.strip()
    if s.startswith("|"):
        s = s[1:]
    if s.endswith("|"):
        s = s[:-1]
    return s.split("|")


def broken_tables(text: str) -> list:
    lines = text.splitlines()
    out = []
    for i, line in enumerate(lines):
        if not line.lstrip().startswith("| ---") and not line.lstrip().startswith("|---"):
            continue
        cs = cells(line)
        if not all(DELIM_CELL.match(c) for c in cs):
            out.append(f"{i + 1}: delimiter row carries a non-delimiter cell")
            continue
        nxt = lines[i + 1] if i + 1 < len(lines) else ""
        if nxt.lstrip().startswith("<!--"):
            out.append(f"{i + 2}: HTML comment line directly after the delimiter row ends the table")
    return out


class TableGrid(unittest.TestCase):
    def test_control_checker_accepts_a_clean_table_and_flags_both_shapes(self):
        ok = "| a | b |\n| --- | --- |\n| 1 | 2 |\n"
        self.assertEqual(broken_tables(ok), [])
        self.assertEqual(len(broken_tables("| a |\n| --- |\n<!-- x -->\n| 1 |\n")), 1)
        self.assertEqual(len(broken_tables("| a |\n| --- | <!-- x -->\n| 1 |\n")), 1)
        self.assertIn("| --- |", DOC.read_text(encoding="utf-8"))

    @unittest.expectedFailure  # backlog 622e33ca: open defect, remove when fixed
    def test_autoflow_verdict_audit_tables_keep_their_grid(self):
        bad = broken_tables(DOC.read_text(encoding="utf-8"))
        self.assertEqual(bad, [], "tables whose grid rendering is broken:\n" + "\n".join(bad))


if __name__ == "__main__":
    unittest.main()
