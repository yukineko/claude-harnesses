#!/usr/bin/env python3
"""Repro test for backlog 65bfb279: no hook runs the scripts/ unittest suites.

scripts/check-workspace-tests.py exists to run them (its docstring says no git hook
ran them), but no file under .githooks/ invokes it, nor `unittest` directly. The
item measured suites red for 11-33 days with nobody told.

The predicted symptom is live at f764bfeb: scripts/test_gate_bypass.py
AmendIsNotABypass fails at its seed commit ("scripts/check-fail-open-diff.py is
missing") because its SCANNERS fixture lags .githooks/pre-commit — a red no gate
surfaced.

Open defect; expectedFailure on the wiring assertion.
"""

import re
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
HOOKS = REPO / ".githooks"


def _code_lines(text):
    return [l for l in text.splitlines() if not l.lstrip().startswith("#")]


class ScriptsSuitesAreGated(unittest.TestCase):
    @unittest.expectedFailure
    def test_some_hook_runs_the_scripts_suites(self):
        hits = []
        for p in sorted(HOOKS.iterdir()):
            if not p.is_file():
                continue
            for l in _code_lines(p.read_text()):
                if "check-workspace-tests.py" in l or re.search(r"\bunittest\b", l):
                    hits.append("%s: %s" % (p.name, l.strip()))
        self.assertTrue(
            hits,
            "no .githooks file (outside comments) invokes check-workspace-tests.py "
            "or unittest, so a red scripts/ suite reaches nobody",
        )


if __name__ == "__main__":
    unittest.main()
