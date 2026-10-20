#!/usr/bin/env python3
"""backlog 9bee4b86: overwatch's test suite cannot pass outside a git checkout,
so cargo-mutants (which builds and tests a scratch COPY with no .git) fails the
unmutated baseline ("cargo test failed in an unmutated tree, so no mutants were
tested", exit 4) and overwatch's kill-rate cannot be measured.

The audit identified the env-dependent test: tests/rollout_gate_crates.rs
shells out to scripts/tests/canary-gate-crates.sh, which fails with
`fatal: not a git repository` there. This test reproduces the cargo-mutants
condition cheaply by running that one test with GIT_DIR pointing at a
non-repository (git then answers exactly "not a git repository"), and fails
while the overwatch test does not pass in that condition.

The canary-gate-crates.sh harness only ever runs rollout-plugins.sh in
--dry-run against a temp cache/registry (its own header), as the existing
`cargo test -p overwatch` already does.

Run:  python3 -m unittest scripts.test_backlog_9bee4b86   (needs cargo; slow on a cold build)
"""
import os
import shutil
import subprocess
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent


def run_overwatch_test(extra_env):
    env = dict(os.environ)
    env.update(extra_env)
    env.setdefault("CARGO_BUILD_JOBS", "2")
    return subprocess.run(
        ["cargo", "test", "-p", "overwatch", "--test", "rollout_gate_crates"],
        cwd=REPO, env=env, capture_output=True, text=True)


@unittest.skipUnless(shutil.which("cargo"), "cargo not available")
class OverwatchTestsOutsideGitCheckout(unittest.TestCase):
    @unittest.expectedFailure
    def test_rollout_gate_crates_passes_without_a_git_repository(self):
        """backlog 9bee4b86: open defect (RED observed)."""
        r = run_overwatch_test({"GIT_DIR": "/nonexistent-backlog-9bee4b86"})
        self.assertEqual(
            r.returncode, 0,
            "overwatch's rollout_gate_crates test needs a real git checkout "
            "(cargo-mutants' scratch copy has none):\n" + r.stdout[-2000:])


if __name__ == "__main__":
    unittest.main()
