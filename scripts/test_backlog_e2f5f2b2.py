#!/usr/bin/env python3
"""Repro test for backlog e2f5f2b2: .githooks/pre-push prose says ONE check blocks.

At f764bfeb the header (line 6) was updated to "three blocking checks", but the
body still says "ONE check blocks: ..." and labels the ledger check "the one
blocking check", while later sections are headed "the second blocking check: the
compiler" and "the third blocking check: the trybuild type contracts". Prose that
contradicts behaviour misleads the next reader (CLAUDE.md 4).

The test counts the ordinal "blocking check" section headers (observable
structure) and requires that no comment claims a single blocking check while
there are several. Open; expectedFailure.
"""

import re
import unittest
from pathlib import Path

PRE_PUSH = Path(__file__).resolve().parent.parent / ".githooks" / "pre-push"

SINGULAR = re.compile(r"\bONE check blocks\b|\bthe one blocking check\b|\bone blocking check\b", re.I)
ORDINAL_HEADER = re.compile(
    r"^\s*# --- the (?:one|first|second|third|fourth|fifth) blocking check", re.I | re.M
)


class PrePushProseMatchesStructure(unittest.TestCase):
    def test_control_several_blocking_sections_exist(self):
        text = PRE_PUSH.read_text()
        self.assertGreaterEqual(len(ORDINAL_HEADER.findall(text)), 2)

    @unittest.expectedFailure
    def test_no_single_blocking_check_claim(self):
        text = PRE_PUSH.read_text()
        n = len(ORDINAL_HEADER.findall(text))
        claims = [
            "%d: %s" % (i, l.strip())
            for i, l in enumerate(text.splitlines(), 1)
            if l.lstrip().startswith("#") and SINGULAR.search(l)
        ]
        self.assertEqual(
            claims,
            [],
            "pre-push has %d blocking-check sections but its prose still claims one" % n,
        )


if __name__ == "__main__":
    unittest.main()
