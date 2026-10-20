#!/usr/bin/env python3
"""RED repro for backlog 53897d41.

53897d41: docs cite the condukt SKILL.md by LINE NUMBER
(`crates/condukt/skills/condukt/SKILL.md:<N>` followed by the quoted text), so
inserting anything above a cited line silently re-points every later
citation at unrelated text (the doc-claims gate then catches it, but every
SKILL.md edit costs a re-anchoring pass).

Pinned here:
  * control (GREEN today): every line citation currently resolves — the cited
    line holds the quoted snippet. Observed, not assumed.
  * defect (RED): the citations do not survive a one-line insertion at the
    top of SKILL.md. Citations anchored on a heading / fixed marker instead of
    a line number make this GREEN; remove the expectedFailure then.
"""

from __future__ import annotations

import glob
import os
import re
import unittest

SCRIPTS = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(SCRIPTS)
SKILL_REL = "crates/condukt/skills/condukt/SKILL.md"
SKILL = os.path.join(REPO, SKILL_REL)

# `<path>:<N>` then a quoted snippet in "..." or `...`. After the digits comes
# either the closing backtick of a code span plus spaces, or spaces, or the
# quote itself — so the text BETWEEN two code spans is never taken as a quote.
CITE = re.compile(
    re.escape(SKILL_REL) + r":(\d+)(?:`\s*|\s+|(?=\"))(?:\"([^\"\n]+)\"|`([^`\n]+)`)"
)


def citations() -> list[tuple[str, int, int, str]]:
    out = []
    for doc in sorted(glob.glob(os.path.join(REPO, "docs", "**", "*.md"), recursive=True)):
        with open(doc, encoding="utf-8") as f:
            for i, line in enumerate(f, 1):
                for m in CITE.finditer(line):
                    quote = m.group(2) or m.group(3)
                    out.append((os.path.relpath(doc, REPO), i, int(m.group(1)), quote))
    return out


def unresolved(lines: list[str]) -> list[str]:
    bad = []
    for doc, doc_line, n, quote in citations():
        target = lines[n - 1] if 0 < n <= len(lines) else ""
        if quote.strip() not in target:
            bad.append(
                f"{doc}:{doc_line} cites SKILL.md:{n} {quote!r}; line {n} is {target.strip()[:100]!r}"
            )
    return bad


def skill_lines() -> list[str]:
    with open(SKILL, encoding="utf-8") as f:
        return f.read().split("\n")


class SkillLineCitations(unittest.TestCase):
    def test_citations_resolve_today(self) -> None:
        self.assertGreaterEqual(len(citations()), 5, citations())
        bad = unresolved(skill_lines())
        self.assertEqual(bad, [], "\n".join(bad))

    @unittest.expectedFailure  # backlog 53897d41: open defect
    def test_citations_survive_an_insertion_above(self) -> None:
        shifted = ["<!-- a line inserted at the top -->"] + skill_lines()
        bad = unresolved(shifted)
        self.assertEqual(
            bad,
            [],
            f"{len(bad)} SKILL.md line citation(s) break after a ONE-line insertion "
            "at the top:\n" + "\n".join(bad),
        )


if __name__ == "__main__":
    unittest.main()
