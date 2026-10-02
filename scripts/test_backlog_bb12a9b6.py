#!/usr/bin/env python3
"""Repro for backlog bb12a9b6 (second symptom): check-plugin-rollout.py's source
walk ignores .gitignore, so an untracked, gitignored per-platform binary left in
crates/<name>/bin/ is treated as payload and byte-compared against the deployed
one.  A gitignored file is not part of the crate, so `_walk_files` must not list
it.  Currently it does.
"""
import importlib.util
import os
import subprocess
import tempfile
import unittest
from pathlib import Path

_HERE = Path(__file__).resolve().parent
_SPEC = importlib.util.spec_from_file_location(
    "check_plugin_rollout", _HERE / "check-plugin-rollout.py"
)
cpr = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(cpr)


class Bb12a9b6(unittest.TestCase):
    @unittest.expectedFailure  # backlog bb12a9b6: open defect; remove when fixed
    def test_gitignored_untracked_binary_is_not_source_payload(self):
        with tempfile.TemporaryDirectory() as tmp:
            subprocess.run(["git", "init", "-q", tmp], check=True)
            Path(tmp, ".gitignore").write_text("crates/*/bin/*-darwin-arm64\n")
            crate = Path(tmp, "crates", "x")
            (crate / "bin").mkdir(parents=True)
            (crate / "bin" / "x").write_text("#!/bin/sh\n")
            (crate / "bin" / "x-darwin-arm64").write_bytes(b"stale hand-built binary")
            ign = subprocess.run(
                ["git", "check-ignore", "crates/x/bin/x-darwin-arm64"],
                cwd=tmp, capture_output=True, text=True,
            )
            self.assertEqual(ign.returncode, 0, "fixture: file must be gitignored")
            files = cpr._walk_files(str(crate))
            self.assertNotIn(
                os.path.join("bin", "x-darwin-arm64"),
                files,
                "gitignored untracked file is listed as crate source payload",
            )


if __name__ == "__main__":
    unittest.main()
