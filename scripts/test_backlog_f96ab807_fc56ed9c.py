#!/usr/bin/env python3
"""Shared-failure pin for backlog f96ab807 (closed as DUPLICATE) and fc56ed9c.

Both tickets: condukt's PostToolUse `editgate` returns a verdict
(`{"decision":"block",...}`) yet is dispatched through `run_hook`, which
catches every panic and exits 0 -- so a crashed gate is indistinguishable from
a clean one (CLAUDE.md 1/3: a verdict-bearing hook belongs on a fail-closed
barrier). f96ab807 additionally names the `run_editgate` docstring that
justifies this with "it can never break a turn" (the phrase CLAUDE.md 1 calls a
red flag on a verdict path); a migration must fix that prose in the same commit.

The ignored tests are RED while fc56ed9c is open (run with RUN_IGNORED=1).
The control proves the parser finds the Editgate arm at all.
"""

from __future__ import annotations

import os
import re
import unittest

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
MAIN = os.path.join(REPO, "crates", "condukt", "src", "main.rs")
IGNORED = unittest.skipUnless(
    os.environ.get("RUN_IGNORED"), "fc56ed9c open: editgate dispatched via run_hook"
)


def _src() -> str:
    with open(MAIN, encoding="utf-8") as f:
        return f.read()


def _editgate_arm() -> str:
    m = re.search(r"Command::Editgate\s*=>\s*([A-Za-z_:]+)\s*\(", _src())
    if m is None:
        raise AssertionError("Command::Editgate dispatch arm not found")
    return m.group(1)


def _run_editgate_doc() -> str:
    m = re.search(r"((?:^///.*\n)+)fn run_editgate\(", _src(), re.M)
    if m is None:
        raise AssertionError("run_editgate docstring not found")
    return m.group(1)


class BacklogF96ab807Fc56ed9c(unittest.TestCase):
    def test_control_dispatch_arm_is_found(self):
        self.assertTrue(_editgate_arm())

    @IGNORED
    def test_editgate_is_not_dispatched_through_the_panic_swallowing_run_hook(self):
        self.assertNotEqual(_editgate_arm(), "run_hook")

    @IGNORED
    def test_run_editgate_doc_does_not_justify_by_never_breaking_the_turn(self):
        self.assertNotIn("never break a turn", _run_editgate_doc().replace("\n/// ", " "))


if __name__ == "__main__":
    unittest.main()
