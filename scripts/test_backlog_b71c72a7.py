#!/usr/bin/env python3
"""Regression test for backlog b71c72a7.

fugu-router's `store::tests::import_episodes_*` / `dedup_episodes_*` used fixed
`std::env::temp_dir().join("fugu-router-import-ep-test")` paths, so two
`cargo test` processes running at the same time (the CLAUDE.md §8 parallel
worktree sessions) appended into the same src.jsonl and broke each other
(`left: 4, right: 2`). Fixed in f502dccd (per-process + per-call TestDir).

This test builds the fugu-router bin unit-test binary once and runs PAIRS of it
concurrently, restricted to the formerly-colliding tests, asserting every run
passes. Against the fixed-path defect it fails (observed by temporarily
reverting TestDir::new to a fixed path).

    python3 scripts/test_backlog_b71c72a7.py
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import unittest
from pathlib import Path

_REPO = Path(__file__).resolve().parent.parent
ITERATIONS = int(os.environ.get("B71C72A7_ITERATIONS", "25"))
FILTER = "store::tests::"


def _test_binary() -> str:
    cargo = shutil.which("cargo") or str(Path.home() / ".cargo/bin/cargo")
    env = dict(os.environ)
    env.setdefault("CARGO_BUILD_JOBS", "2")
    p = subprocess.run(
        [cargo, "test", "-p", "fugu-router", "--bin", "fugu-router", "--no-run",
         "--message-format=json"],
        cwd=_REPO, env=env, capture_output=True, text=True,
    )
    if p.returncode != 0:
        raise AssertionError("build failed:\n" + p.stderr[-3000:])
    exe = None
    for line in p.stdout.splitlines():
        try:
            msg = json.loads(line)
        except json.JSONDecodeError:
            continue
        if (msg.get("reason") == "compiler-artifact"
                and msg.get("target", {}).get("name") == "fugu-router"
                and msg.get("profile", {}).get("test")
                and msg.get("executable")):
            exe = msg["executable"]
    if exe is None:
        raise AssertionError("no fugu-router bin test executable found")
    return exe


class ConcurrentTestRunsDoNotCollide(unittest.TestCase):
    def test_two_concurrent_store_test_runs_both_pass(self):
        exe = _test_binary()
        failures = []
        for i in range(ITERATIONS):
            procs = [
                subprocess.Popen([exe, FILTER, "--test-threads=4"],
                                 stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                 text=True)
                for _ in range(2)
            ]
            for p in procs:
                out, _ = p.communicate()
                # Anti-vacuity: a filter matching nothing would "pass".
                self.assertIn("import_episodes_deduplicates ... ok", out
                              if p.returncode == 0 else
                              "import_episodes_deduplicates ... ok")
                if p.returncode != 0:
                    failures.append((i, out[-1500:]))
        self.assertEqual(
            failures, [],
            "%d of %d concurrent runs failed; first:\n%s" % (
                len(failures), 2 * ITERATIONS,
                failures[0][1] if failures else ""),
        )


if __name__ == "__main__":
    unittest.main()
