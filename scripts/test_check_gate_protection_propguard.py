#!/usr/bin/env python3
"""Backlog 3a8e3b73: propguard adoption. On the real tree, propguard must be
off the PENDING baseline (it declares) and the checker must pass."""
import subprocess
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
BASELINE = REPO / "scripts/check-gate-protection.baseline"
SCRIPT = REPO / "scripts/check-gate-protection.py"


class PropguardAdopted(unittest.TestCase):
    def test_propguard_not_pending(self):
        names = [
            l.strip()
            for l in BASELINE.read_text().splitlines()
            if l.strip() and not l.lstrip().startswith("#")
        ]
        self.assertNotIn("propguard", names)

    def test_checker_passes_on_real_tree(self):
        r = subprocess.run(
            ["python3", str(SCRIPT), "--repo", str(REPO)], capture_output=True, text=True
        )
        self.assertEqual(r.returncode, 0, f"stdout={r.stdout!r} stderr={r.stderr!r}")


if __name__ == "__main__":
    unittest.main()
