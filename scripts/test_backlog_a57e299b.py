#!/usr/bin/env python3
"""backlog a57e299b: test_check_launcher_exec_bit's RealRepoState scope test
must be GREEN on the real repo.

Observed 2026-10-03: `AssertionError: 40 != 39` — the expected launcher count was
a hand-pinned 39 that went stale when a plugin was added. Fixed in the test file
itself by 162389a6 (backlog 26f6dd72: the expected set is derived from each
plugin crate's Cargo.toml bin targets). Because the fix lives IN that suite
file, this wrapper runs the suite as it stands in the tree under test: RED at
162389a6^ (2ac122ac), GREEN after.

Repro:  python3 -m pytest scripts/test_backlog_a57e299b.py
"""

import subprocess
import sys
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
SUITE = HERE / "test_check_launcher_exec_bit.py"
TEST = "test_the_scope_finds_every_plugin_that_ships_a_binary"


class LauncherScopeTestIsGreen(unittest.TestCase):
    def test_scope_test_passes_on_the_real_repo(self):
        p = subprocess.run(
            [sys.executable, "-m", "pytest", "-q", "-p", "no:cacheprovider",
             str(SUITE), "-k", TEST],
            cwd=str(HERE), capture_output=True, text=True, timeout=600,
        )
        self.assertEqual(p.returncode, 0, p.stdout[-3000:] + p.stderr[-2000:])
        self.assertIn("1 passed", p.stdout, p.stdout[-3000:])


if __name__ == "__main__":
    unittest.main()
