#!/usr/bin/env python3
"""Repro for backlog a57fa847: `cargo test -p autoflow` implicitly requires the
`backlog` binary to have been built beforehand. crates/autoflow/tests/
precompact_lock.rs resolves it as a sibling of the test executable
(target/<profile>/backlog) and asserts it exists, but autoflow declares no
dependency that would make cargo build it, so on a clean target the two
precompact tests FAIL with "expected a built `backlog` binary ... run
`cargo build -p backlog`". An environment precondition is indistinguishable
from a regression.

Clean-target simulation: build the precompact_lock test executable with cargo,
copy it into a fresh <tmp>/debug/deps/ (exactly the layout it resolves from,
with no `backlog` sibling), and run it. A test suite that declares its own
prerequisites passes there.

Slow (builds autoflow's tests): opt in with RUN_CARGO_REPROS=1.
"""
import json
import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent


def build_test_exe() -> Path:
    r = subprocess.run(["cargo", "test", "-p", "autoflow", "--test", "precompact_lock",
                        "--no-run", "--message-format=json"],
                       cwd=REPO, capture_output=True, text=True)
    if r.returncode != 0:
        raise AssertionError(f"build failed -- undetermined:\n{r.stderr[-2000:]}")
    for line in r.stdout.splitlines():
        try:
            msg = json.loads(line)
        except json.JSONDecodeError:
            continue
        if msg.get("reason") == "compiler-artifact" and msg.get("executable") \
                and msg.get("target", {}).get("name") == "precompact_lock":
            return Path(msg["executable"])
    raise AssertionError("no precompact_lock executable reported -- undetermined")


@unittest.skipUnless(os.environ.get("RUN_CARGO_REPROS") == "1", "slow; set RUN_CARGO_REPROS=1")
class CleanTarget(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.TemporaryDirectory()
        deps = Path(cls.tmp.name) / "debug" / "deps"
        deps.mkdir(parents=True)
        cls.exe = deps / "precompact_lock-clean"
        shutil.copy2(build_test_exe(), cls.exe)

    @classmethod
    def tearDownClass(cls):
        cls.tmp.cleanup()

    def test_control_both_precompact_tests_are_present(self):
        r = subprocess.run([str(self.exe), "--list"], capture_output=True, text=True)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("precompact_does_not_resume_for_an_unrelated_projects_lock", r.stdout)
        self.assertIn("precompact_resumes_flow_only_when_this_session_holds_its_own_project_lock",
                      r.stdout)

    @unittest.expectedFailure  # backlog a57fa847: open defect, remove when fixed
    def test_suite_passes_without_a_prebuilt_backlog_sibling(self):
        r = subprocess.run([str(self.exe)], capture_output=True, text=True)
        self.assertEqual(r.returncode, 0, (r.stdout + r.stderr)[-2500:])


if __name__ == "__main__":
    unittest.main()
