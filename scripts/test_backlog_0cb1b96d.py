#!/usr/bin/env python3
"""backlog 0cb1b96d: tests leave `-tmpXXXX-<hash>` project dirs in the REAL
~/.overwatch (1256 of them on 2026-10-02, ~80% of the entries).

The newest debris dirs each hold one review_findings.jsonl row such as
`{"finding_id":"specguard:spec-drift:src","source":"specguard",...,
"file":"reports/2026-01-01.md"}` — written by specguard's integration tests,
which spawn the specguard binary from a tempdir repo WITHOUT sandboxing HOME,
so specguard's overwatch::store::record_finding lands under the user's HOME.

Reproduction without touching the real HOME: run one such specguard test with
HOME pointed at a fresh sandbox (CARGO_HOME / RUSTUP_HOME pinned to the real
toolchain) and observe that it writes a `-tmp*` project dir into
<sandbox>/.overwatch — i.e. the test writes into whatever HOME it inherits.

Run:  python3 -m unittest scripts.test_backlog_0cb1b96d   (needs cargo; slow on a cold build)
"""
import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
TEST = "run_with_findings_writes_report_and_sentinel"


@unittest.skipUnless(shutil.which("cargo"), "cargo not available")
class TestsDoNotWriteIntoInheritedHome(unittest.TestCase):
    @unittest.expectedFailure
    def test_specguard_integration_test_leaves_no_overwatch_dir_in_home(self):
        """backlog 0cb1b96d: open defect (RED observed)."""
        real_home = Path.home()
        with tempfile.TemporaryDirectory() as sandbox:
            env = dict(os.environ)
            env["CARGO_HOME"] = os.environ.get("CARGO_HOME", str(real_home / ".cargo"))
            env["RUSTUP_HOME"] = os.environ.get("RUSTUP_HOME", str(real_home / ".rustup"))
            env["HOME"] = sandbox
            env.setdefault("CARGO_BUILD_JOBS", "2")
            r = subprocess.run(
                ["cargo", "test", "-p", "specguard", "--test", "integration",
                 TEST, "--", "--exact"],
                cwd=REPO, env=env, capture_output=True, text=True)
            self.assertIn("1 passed", r.stdout,
                          f"precondition: the specguard test ran: {r.stdout[-1500:]} {r.stderr[-1500:]}")
            ow = Path(sandbox) / ".overwatch"
            leaked = sorted(p.name for p in ow.iterdir()) if ow.is_dir() else []
            self.assertEqual(
                leaked, [],
                f"specguard's {TEST} wrote overwatch project dirs into the HOME it "
                f"inherited (the real ~/.overwatch in a normal run): {leaked}")


if __name__ == "__main__":
    unittest.main()
