#!/usr/bin/env python3
"""Repro for backlog ec2ec82a: 15 propguard unit tests in crates/propguard/src/
git.rs begin with `if !git_available() { eprintln!("skipping..."); return; }`
where git_available() is `...output().map(|o| o.status.success())
.unwrap_or(false)`. When git cannot be launched, those tests verify nothing and
are COUNTED AS PASSED -- a test that could not run reports ok (CLAUDE.md
section 2: an assert that verifies nothing is fixed as the spec; section 3:
unwrap_or defaults to the permissive side). Reachable in practice: an
unaccepted Xcode license makes git exit 69 on this host class.

The test builds propguard's bin unit tests, runs ONE git-dependent test with a
PATH that cannot resolve `git`, and requires the harness NOT to report it as
passed (fail or ignored are both honest; "ok" is not).

Slow (builds propguard's unit tests): opt in with RUN_CARGO_REPROS=1.
"""
import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
TEST = "git::tests::clean_repo_is_empty_files_not_failed"


def build_unit_exe() -> Path:
    r = subprocess.run(["cargo", "test", "-p", "propguard", "--bin", "propguard",
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
                and msg.get("profile", {}).get("test"):
            return Path(msg["executable"])
    raise AssertionError("no propguard unit-test executable reported -- undetermined")


@unittest.skipUnless(os.environ.get("RUN_CARGO_REPROS") == "1", "slow; set RUN_CARGO_REPROS=1")
class NoGit(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.exe = build_unit_exe()
        cls.empty = tempfile.TemporaryDirectory()

    @classmethod
    def tearDownClass(cls):
        cls.empty.cleanup()

    def run_test(self, path: str):
        env = dict(os.environ, PATH=path)
        return subprocess.run([str(self.exe), "--exact", TEST, "--nocapture"],
                              env=env, capture_output=True, text=True)

    def test_control_with_git_the_test_runs_its_body(self):
        r = self.run_test(os.environ["PATH"])
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertIn("1 passed", r.stdout)
        self.assertNotIn("skipping: git not available", r.stdout + r.stderr)

    @unittest.expectedFailure  # backlog ec2ec82a: open defect, remove when fixed
    def test_without_git_the_test_is_not_reported_passed(self):
        r = self.run_test(self.empty.name)  # PATH with no `git` on it
        out = r.stdout + r.stderr
        self.assertIn("skipping: git not available", out, "precondition: git was unreachable")
        self.assertNotIn("1 passed", r.stdout,
                         f"a test that verified nothing was counted as passed:\n{out[-1500:]}")


if __name__ == "__main__":
    unittest.main()
