#!/usr/bin/env python3
"""Shared-failure pin for backlog a35997cb and d4d74fe2 (both closed as
DUPLICATE) and e494a8a3 (the surviving item).

All three: condukt's unit test
`diffrisk_record::tests::every_invocation_is_journaled_even_when_nothing_is_recorded`
intermittently fails under the full parallel unit suite with
`one record per invocation` left 1 / right 2 (a journal record goes missing),
while it passes alone or with --test-threads=1.

The ignored test builds the condukt unit-test binary once and runs the WHOLE
suite (default parallelism) RUNS times; it is RED while e494a8a3 is open
(run with RUN_IGNORED=1). Observed 2026-10-02 at e672436b: the test failed in
4 of 6 full-suite runs of the prebuilt binary.
"""

from __future__ import annotations

import json
import os
import subprocess
import unittest

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
TEST = "diffrisk_record::tests::every_invocation_is_journaled_even_when_nothing_is_recorded"
RUNS = int(os.environ.get("DIFFRISK_FLAKE_RUNS", "6"))
IGNORED = unittest.skipUnless(
    os.environ.get("RUN_IGNORED"), "e494a8a3 open: diffrisk journal flake under parallel suite"
)


def _unit_test_binary() -> str:
    cargo = os.environ.get("CARGO", "cargo")
    p = subprocess.run(
        [cargo, "test", "-p", "condukt", "--bin", "condukt", "--no-run",
         "--message-format=json"],
        cwd=REPO, capture_output=True, text=True,
    )
    if p.returncode != 0:
        raise AssertionError(f"cargo test --no-run failed: {p.stderr[-2000:]}")
    for line in p.stdout.splitlines():
        try:
            msg = json.loads(line)
        except json.JSONDecodeError:
            continue
        if (msg.get("reason") == "compiler-artifact" and msg.get("executable")
                and msg.get("target", {}).get("name") == "condukt"
                and msg.get("profile", {}).get("test")):
            return msg["executable"]
    raise AssertionError("no condukt unit-test executable in cargo output")


class BacklogE494a8a3DiffriskFlake(unittest.TestCase):
    @IGNORED
    def test_journaling_test_never_fails_under_the_parallel_suite(self):
        exe = _unit_test_binary()
        failures = 0
        for _ in range(RUNS):
            p = subprocess.run([exe], cwd=REPO, capture_output=True, text=True)
            if f"test {TEST} ... FAILED" in p.stdout:
                failures += 1
        self.assertEqual(failures, 0, f"{TEST} failed in {failures}/{RUNS} full-suite runs")


if __name__ == "__main__":
    unittest.main()
