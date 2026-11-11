"""Repro for scout 2026-11-12 F3: cap-target-dir.sh writes .cap-cleaned-at even when `cargo clean` failed.

cap-target-dir.sh runs `cargo clean || true` and then unconditionally writes
<target>/.cap-cleaned-at. The marker is documented as evidence that a clean
HAPPENED (test-changed-crates.sh reads it to tell "binary deleted under my run"
from a genuine failure), so a failed clean must not leave it.

Run: RUN_BACKLOG_REPROS=1 python3 scripts/test_scout_20261112_f3_cap_clean_failed_repro.py
Remove the skip decorator in the commit that fixes the defect. Stdlib only;
`cargo` is a PATH stub, nothing touches a real target dir.
"""

from __future__ import annotations

import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
SCRIPT = HERE / "cap-target-dir.sh"

OPEN = unittest.skipUnless(
    os.environ.get("RUN_BACKLOG_REPROS"),
    "scout 2026-11-12 F3: open defect, set RUN_BACKLOG_REPROS=1 to run; remove skip when fixed",
)


class CapCleanMarker(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="f3-cap-clean-")).resolve()
        self.addCleanup(shutil.rmtree, self.tmp, ignore_errors=True)
        self.target = self.tmp / "target"
        self.target.mkdir()
        # ~3MB so `du -sm` > cap of 1.
        with open(self.target / "blob", "wb") as f:
            f.write(b"x" * (3 * 1024 * 1024))
        self.bin = self.tmp / "bin"
        self.bin.mkdir()

    def _run(self, clean_rc: int):
        stub = self.bin / "cargo"
        with open(stub, "w") as f:
            f.write(
                "#!/usr/bin/env bash\n"
                'case "$1" in\n'
                f'  metadata) printf \'{{"target_directory":"%s"}}\' "{self.target}"; exit 0 ;;\n'
                f"  clean) exit {clean_rc} ;;\n"
                "esac\nexit 0\n"
            )
        stub.chmod(0o755)
        env = dict(
            os.environ,
            PATH=f"{self.bin}{os.pathsep}{os.environ['PATH']}",
            CARGO_TARGET_CAP_MB="1",
        )
        return subprocess.run(
            ["bash", str(SCRIPT)], env=env, capture_output=True, text=True
        )

    def test_control_successful_clean_writes_marker(self):
        r = self._run(0)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertTrue(
            (self.target / ".cap-cleaned-at").exists(),
            "control: clean succeeded, marker should be written",
        )

    @OPEN
    def test_failed_clean_must_not_write_marker(self):
        r = self._run(1)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertTrue((self.target / "blob").exists(), "stub clean deletes nothing")
        self.assertFalse(
            (self.target / ".cap-cleaned-at").exists(),
            "cargo clean exited 1 and deleted nothing, yet .cap-cleaned-at was written "
            "(cap-target-dir.sh `cargo clean || true` then unconditional marker write)",
        )


if __name__ == "__main__":
    unittest.main()
