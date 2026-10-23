#!/usr/bin/env python3
"""backlog dacb9dc1: scripts/test_backlog_6267bfbe_36c936bc.py must be GREEN.

Observed 2026-10-20: the suite reported 1 failed ("Unexpected success") because
test_ff_onto_merge_gated_in_other_worktree_is_not_recorded kept its
expectedFailure marker after the defect it pinned (36c936bc) was fixed by
1472b9f4 (backlog 1d2446de). The fix is the marker's removal, which lives IN
that suite file, so this wrapper runs the suite file as it stands in the tree
under test: RED wherever the stale marker is still present, GREEN once removed.

Repro:  python3 -m pytest scripts/test_backlog_dacb9dc1.py
"""

import subprocess
import sys
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
SUITE = HERE / "test_backlog_6267bfbe_36c936bc.py"


class SuiteIsGreen(unittest.TestCase):
    def test_6267bfbe_36c936bc_suite_passes(self):
        p = subprocess.run(
            [sys.executable, "-m", "pytest", "-q", "-p", "no:cacheprovider", str(SUITE)],
            cwd=str(HERE), capture_output=True, text=True, timeout=600,
        )
        self.assertEqual(p.returncode, 0, p.stdout[-3000:] + p.stderr[-2000:])
        self.assertIn("2 passed", p.stdout, p.stdout[-3000:])


if __name__ == "__main__":
    unittest.main()
