#!/usr/bin/env python3
"""Backlog 396b6089 — RED repro.

scripts/check-plugin-versions.py documents "2 if zero plugins were found to
check (an empty set is 'cannot determine', not 'consistent')". Run from a
directory that has .claude-plugin/marketplace.json but NO crates/, it instead
dies with an uncaught FileNotFoundError traceback (exit 1, the "drift" code),
so "could not even look" is indistinguishable from "found drift".

Run: python3 -m pytest --runxfail scripts/test_backlog_396b6089.py
"""

import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SRC = Path(__file__).resolve().parent.parent
SCRIPT = SRC / "scripts" / "check-plugin-versions.py"


class PluginVersionsWithoutCratesDir(unittest.TestCase):
    def _run(self, with_crates):
        root = Path(tempfile.mkdtemp(prefix="396b6089-"))
        self.addCleanup(shutil.rmtree, root, True)
        (root / ".claude-plugin").mkdir()
        (root / ".claude-plugin" / "marketplace.json").write_text(
            '{"plugins": []}', encoding="utf-8"
        )
        if with_crates:
            (root / "crates").mkdir()
        return subprocess.run(
            [sys.executable, str(SCRIPT)],
            cwd=root,
            capture_output=True,
            text=True,
            stdin=subprocess.DEVNULL,
        )

    def test_control_empty_crates_dir_is_undetermined_exit_2(self):
        p = self._run(with_crates=True)
        self.assertEqual(p.returncode, 2, p.stdout + p.stderr)

    @unittest.expectedFailure
    def test_missing_crates_dir_is_undetermined_exit_2_not_traceback(self):
        p = self._run(with_crates=False)
        self.assertNotIn("Traceback", p.stderr, p.stderr)
        self.assertEqual(p.returncode, 2, p.stdout + p.stderr)


if __name__ == "__main__":
    unittest.main()
