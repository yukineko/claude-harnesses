#!/usr/bin/env python3
"""Repro test for backlog 5eb1f127: gates written fail-closed that no hook calls.

A gate that never runs is not red, it is dark. Each script below must be settled
one of three ways: (a) invoked from a file under .githooks/, (b) declared as
intentionally unwired in a machine-readable scripts/*.json entry that names it and
carries a reason, parked_at and revisit (the shape scripts/parked-plugins.json uses),
or (c) removed. An undeclared unwired gate must not be read as "probably on
purpose" (CLAUDE.md, 'parked on purpose is declared').

docs/gate-taxonomy.md describes check-fail-open-mutation.py and
check-bench-regression.py as CI-triggered, but CI is gone (no .github/ directory;
CLAUDE.md 7), so that prose is not a declaration of anything current.

Open defect at f764bfeb: all five are unwired and undeclared. expectedFailure.
"""

import json
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
HOOKS = REPO / ".githooks"
CANDIDATES = [
    "check-unwind.sh",
    "check-versions.sh",
    "check-bench-regression.py",
    "check-fail-open-mutation.py",
    "validate-manifests.py",
]


def _declared(name):
    for p in sorted((REPO / "scripts").glob("*.json")):
        try:
            data = json.loads(p.read_text())
        except (OSError, ValueError):
            continue
        stack = [data]
        while stack:
            node = stack.pop()
            if isinstance(node, dict):
                blob = json.dumps(node)
                if name in blob and all(k in node for k in ("reason", "parked_at", "revisit")):
                    return True
                stack.extend(node.values())
            elif isinstance(node, list):
                stack.extend(node)
    return False


def unsettled():
    hook_text = "\n".join(p.read_text() for p in HOOKS.iterdir() if p.is_file())
    out = []
    for name in CANDIDATES:
        if not (REPO / "scripts" / name).exists():
            continue  # removed: settled
        if name in hook_text:
            continue  # wired
        if _declared(name):
            continue  # declared intentionally unwired
        out.append(name)
    return out


class EveryGateIsWiredOrDeclared(unittest.TestCase):
    def test_control_clippy_lints_is_wired(self):
        # The sibling the ticket originally listed, since wired; proves the
        # wiring probe can see a real reference.
        hook_text = (HOOKS / "pre-commit").read_text()
        self.assertIn("check-clippy-lints.py", hook_text)

    @unittest.expectedFailure
    def test_no_dark_undeclared_gate(self):
        self.assertEqual(
            unsettled(),
            [],
            "these gate scripts run from no hook and carry no parked declaration "
            "(dark, not red)",
        )


if __name__ == "__main__":
    unittest.main()
