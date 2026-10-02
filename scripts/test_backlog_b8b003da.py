#!/usr/bin/env python3
"""RED repro for backlog b8b003da.

specguard's `forge_integration::ratify_promotes_draft_and_pins_consent` asserts a
happy path that requires a `backlog` binary on the AMBIENT PATH, without
declaring or providing it. On a PATH that has cargo but no `backlog`, the test
fails; on a developer shell that happens to carry the plugin cache's backlog it
passes. Its verdict therefore measures the environment, not the code.

This test runs it on a minimal PATH (cargo's dir + system dirs) and asserts it
passes — i.e. that the test is hermetic. Open defect -> expectedFailure.

    python3 scripts/test_backlog_b8b003da.py
"""

from __future__ import annotations

import os
import shutil
import subprocess
import unittest
from pathlib import Path

_REPO = Path(__file__).resolve().parent.parent


class ForgeIntegrationIsHermetic(unittest.TestCase):
    @unittest.expectedFailure  # backlog b8b003da: open defect, remove when fixed
    def test_ratify_passes_without_backlog_on_path(self):
        cargo = shutil.which("cargo") or str(Path.home() / ".cargo/bin/cargo")
        if not os.path.exists(cargo):
            self.skipTest("cargo not available")
        minimal = os.pathsep.join(
            [str(Path(cargo).parent), "/usr/bin", "/bin", "/usr/sbin", "/sbin"]
        )
        # Precondition (anti-vacuity): backlog really is absent on this PATH.
        self.assertIsNone(
            shutil.which("backlog", path=minimal),
            "precondition: a backlog binary is on the minimal PATH",
        )
        env = dict(os.environ)
        env["PATH"] = minimal
        env.setdefault("CARGO_BUILD_JOBS", "2")
        p = subprocess.run(
            [
                cargo, "test", "-p", "specguard", "--test", "forge_integration",
                "ratify_promotes_draft_and_pins_consent",
            ],
            cwd=_REPO, env=env, capture_output=True, text=True,
        )
        self.assertEqual(
            p.returncode, 0,
            "forge_integration ratify test depends on ambient PATH:\n"
            + p.stdout[-3000:] + p.stderr[-2000:],
        )


if __name__ == "__main__":
    unittest.main()
