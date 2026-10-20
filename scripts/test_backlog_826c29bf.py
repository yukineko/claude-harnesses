#!/usr/bin/env python3
"""Repro test for backlog 826c29bf (umbrella: gate mission = audit against a clear
instruction), done-criterion 6:

  6. agent 側の規範（申告して Ask、別構文で再試行しない）を SKILL / CLAUDE.md に書く。

The item's observed cost: a shell redirect was refused, so the identical write was
re-routed through python and succeeded — the gate protected nothing and taught
evasion. CLAUDE.md 5 says "黙って迂回せず、見えるところに残す" but never names the
specific failure (re-trying the SAME effect through ANOTHER syntax / tool after a
block) nor requires declaring the block verbatim and asking.

The other criteria are covered by the child items (ae4543d5, b5358f58, ad524af9,
cc809395, 73c2c089, 5eb1f127 — each with its own repro); criterion 5 (the autoflow
compass gate) no longer applies: that gate was retired 2026-08-20
(crates/autoflow/src/main.rs "RETIRED 2026-08-20"). This file pins only what the
umbrella itself owns. Fixed: the norm is CLAUDE.md 5 item 4.
"""

import re
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent

# The norm has two parts; both must be stated somewhere an agent reads.
NO_ALT_SYNTAX = re.compile(
    r"別構文|別の構文|another syntax|different syntax|other syntax|same effect (?:via|through)|"
    r"同じ効果を別",
    re.I,
)
DECLARE_AND_ASK = re.compile(r"逐語で申告|申告して\s*Ask|declare (?:the block|it) verbatim", re.I)


def sources():
    yield REPO / "CLAUDE.md"
    yield from sorted(REPO.glob("crates/*/skills/*/SKILL.md"))


class AgentSideNormIsWritten(unittest.TestCase):
    def test_control_patterns_match_the_items_own_wording(self):
        # Wording from the item's notes (user instruction 2026-09-07).
        sample = (
            "block されたら理由を逐語で申告して Ask。"
            "黙って迂回する、あるいは別構文で同じ効果を得るのがバグ。"
        )
        self.assertRegex(sample, NO_ALT_SYNTAX)
        self.assertRegex(sample, DECLARE_AND_ASK)

    def test_no_alt_syntax_and_declare_ask_norm_present(self):
        alt = [str(p.relative_to(REPO)) for p in sources() if NO_ALT_SYNTAX.search(p.read_text())]
        ask = [str(p.relative_to(REPO)) for p in sources() if DECLARE_AND_ASK.search(p.read_text())]
        self.assertTrue(
            alt and ask,
            "criterion 6 unmet: 'do not retry the same effect via another syntax' found in %r, "
            "'declare the block verbatim and Ask' found in %r" % (alt, ask),
        )


if __name__ == "__main__":
    unittest.main()
