#!/usr/bin/env python3
"""Closure regression for backlog 841fb5c8.

841fb5c8: crates/specguard/bin/specguard (a GATE crate's launcher) exited 0
when no per-platform binary was bundled, with the comment "it exits 0 silently
so a hook NEVER breaks the user's turn" -- "checked, nothing to report" and
"never ran" were indistinguishable.

Pinned (launcher copied alone into a temp dir, so no platform binary exists):
  * a CLI verb (`audit`) exits non-zero and says its result is UNKNOWN;
  * the SessionStart verb (`pending`, whose hook appends `|| true` so only
    stdout survives) prints a non-empty notice naming the result UNKNOWN,
    rather than the empty stdout that reads as "no drift pending";
  * the launcher no longer justifies itself with "never breaks the turn".
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import tempfile
import unittest

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
LAUNCHER = os.path.join(REPO, "crates", "specguard", "bin", "specguard")


class Backlog841fb5c8(unittest.TestCase):
    def setUp(self) -> None:
        self.dir = tempfile.mkdtemp(prefix="bl-841fb5c8.")
        self.addCleanup(lambda: shutil.rmtree(self.dir, ignore_errors=True))
        self.bin = os.path.join(self.dir, "specguard")
        shutil.copy(LAUNCHER, self.bin)
        os.chmod(self.bin, 0o755)
        self.assertEqual(
            [f for f in os.listdir(self.dir) if f != "specguard"], [],
            "fixture: no platform binary may sit next to the launcher",
        )

    def _run(self, *args: str) -> subprocess.CompletedProcess:
        return subprocess.run(["/bin/sh", self.bin, *args], capture_output=True, text=True)

    def test_cli_verb_without_binary_is_not_success(self):
        p = self._run("audit")
        self.assertNotEqual(p.returncode, 0, p.stderr)
        self.assertIn("UNKNOWN", p.stderr)

    def test_sessionstart_verb_without_binary_is_not_silent(self):
        p = self._run("pending")
        self.assertTrue(p.stdout.strip(), "empty stdout reads as 'no drift pending'")
        msg = json.loads(p.stdout)
        self.assertIn("UNKNOWN", msg["systemMessage"])
        self.assertIn("UNKNOWN", msg["hookSpecificOutput"]["additionalContext"])

    def test_launcher_does_not_justify_by_never_breaking_the_turn(self):
        with open(LAUNCHER, encoding="utf-8") as f:
            text = f.read().lower()
        self.assertNotIn("never breaks the user's turn", text)


if __name__ == "__main__":
    unittest.main()
