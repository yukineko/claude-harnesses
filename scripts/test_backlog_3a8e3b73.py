#!/usr/bin/env python3
"""Repro tests for backlog 3a8e3b73 (gate protection contract), remaining slices.

Done-criteria (item text): "every gate crate and every scripts/check-*.py exposes
a protection statement naming asset and adversary, gate-taxonomy.md gains that
column, and the statement is machine-pinned". User ruling 2026-09-24 Q1: the
population is harness_core BLOCKING_GATES + ALL scripts/check-*; the proposed
Python form is a PROTECTION dict (protects / against / grounds).

At f764bfeb the Rust half is done (scripts/check-gate-protection.py reports all 7
BLOCKING_GATES DECLARED, rc 0 — kept as a control), but:
  * no scripts/check-*.py declares PROTECTION;
  * docs/gate-taxonomy.md's gate table has no protection column.
Both open; expectedFailure.
"""

import re
import subprocess
import sys
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
TAXONOMY = REPO / "docs" / "gate-taxonomy.md"

_DICT = re.compile(r"^PROTECTION\s*(?::[^=]*)?=\s*\{(.*?)^\}", re.S | re.M)


def _declares(path):
    m = _DICT.search(path.read_text())
    if not m:
        return False
    body = m.group(1)
    return all(
        re.search(r"[\"']%s[\"']\s*:\s*[\"'][^\"']+" % k, body)
        for k in ("protects", "against", "grounds")
    )


class ProtectionContract(unittest.TestCase):
    def test_control_blocking_gates_all_declared(self):
        p = subprocess.run(
            [sys.executable, str(REPO / "scripts" / "check-gate-protection.py")],
            cwd=REPO,
            capture_output=True,
            text=True,
            timeout=120,
        )
        self.assertEqual(p.returncode, 0, p.stdout + p.stderr)
        self.assertNotIn("PENDING", p.stdout)

    @unittest.expectedFailure
    def test_every_check_script_declares_protection(self):
        scripts = sorted((REPO / "scripts").glob("check-*.py"))
        self.assertTrue(scripts, "precondition: there are check scripts")
        missing = [p.name for p in scripts if not _declares(p)]
        self.assertEqual(
            missing,
            [],
            "%d/%d scripts/check-*.py lack a PROTECTION {protects, against, grounds} "
            "statement" % (len(missing), len(scripts)),
        )

    @unittest.expectedFailure
    def test_taxonomy_gate_table_has_protection_column(self):
        headers = [
            l
            for l in TAXONOMY.read_text().splitlines()
            if l.startswith("| ゲート |") or l.lower().startswith("| gate |")
        ]
        self.assertTrue(headers, "precondition: the gate table exists")
        self.assertTrue(
            any(re.search(r"保護|protects|PROTECTS", h) for h in headers),
            "gate-taxonomy.md gate table lacks a protection column: %r" % headers,
        )


if __name__ == "__main__":
    unittest.main()
