#!/usr/bin/env python3
"""Shared-failure pin for backlog 1da483dc (closed as DUPLICATE) and 25692f6e.

Both tickets: the UNGATED COMMIT guidance in .githooks/post-commit tells the
reader to run `python3 scripts/gate-bypass.py --status`, but gate-bypass.py has
no `--status` option (argparse usage error, exit 2).

The ignored test is RED while 25692f6e is open (run with RUN_IGNORED=1): every
`gate-bypass.py --<flag>` the hook cites must be an option gate-bypass.py
accepts. The non-ignored control proves the extractor sees the hook's citations
at all (so the ignored test cannot pass vacuously on an empty set).
"""

from __future__ import annotations

import os
import re
import subprocess
import sys
import unittest

SCRIPTS = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(SCRIPTS)
HOOK = os.path.join(REPO, ".githooks", "post-commit")
GATE = os.path.join(SCRIPTS, "gate-bypass.py")
IGNORED = unittest.skipUnless(
    os.environ.get("RUN_IGNORED"), "25692f6e open: post-commit cites --status"
)


def _cited_flags() -> set[str]:
    text = open(HOOK, encoding="utf-8").read()
    return set(re.findall(r"gate-bypass\.py\s+(--[a-z][a-z-]*)", text))


def _accepted_flags() -> set[str]:
    p = subprocess.run([sys.executable, GATE, "-h"], capture_output=True, text=True)
    if p.returncode != 0:
        raise AssertionError(f"gate-bypass.py -h failed rc={p.returncode}: {p.stderr}")
    return set(re.findall(r"(--[a-z][a-z-]*)", p.stdout))


class Backlog1da483dc25692f6e(unittest.TestCase):
    def test_control_hook_cites_at_least_one_gate_bypass_flag(self):
        self.assertTrue(_cited_flags(), "extractor found no citation in post-commit")

    @IGNORED
    def test_every_flag_the_hook_cites_exists(self):
        missing = _cited_flags() - _accepted_flags()
        self.assertEqual(missing, set(), f"post-commit cites non-existent flags: {missing}")


if __name__ == "__main__":
    unittest.main()
