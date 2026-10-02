#!/usr/bin/env python3
"""Repro for backlog ee273b5e.

guard-maintree-bash.py refuses a `cp` that writes the resolved content into a
file that is CONFLICTED during an in-progress merge on the main checkout.
CLAUDE.md section 8 explicitly permits integration (merge, conflict
resolution) on main, so putting the resolved file in place must be allowed.

Fixture: a throwaway repo (the main checkout, CLAUDE_PROJECT_DIR) with a merge
stopped on a conflict in a.txt. The control shows the same `cp` outside a
merge is refused, so the guard is live and the RED below is specific to the
conflict-resolution case.

Written by an independent auditor, not an implementer.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
import unittest

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
GUARD = os.path.join(REPO, "scripts", "guard-maintree-bash.py")


def _git(cwd: str, *args: str) -> subprocess.CompletedProcess:
    env = dict(os.environ, GIT_CONFIG_NOSYSTEM="1", HOME=cwd)
    return subprocess.run(
        ["git", *args], cwd=cwd, env=env, capture_output=True, text=True
    )


def _must(cwd: str, *args: str) -> None:
    p = _git(cwd, *args)
    if p.returncode != 0:
        raise AssertionError(f"fixture void: git {args}: {p.stdout}{p.stderr}")


def _guard(repo: str, command: str) -> subprocess.CompletedProcess:
    payload = {"tool_name": "Bash", "tool_input": {"command": command}}
    env = dict(os.environ, CLAUDE_PROJECT_DIR=repo)
    return subprocess.run(
        [sys.executable, GUARD],
        cwd=repo,
        env=env,
        input=json.dumps(payload),
        capture_output=True,
        text=True,
    )


class GuardAllowsConflictResolution(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        base = os.path.realpath(self._tmp.name)
        self.repo = os.path.join(base, "repo")
        os.makedirs(self.repo)
        self.resolved = os.path.join(base, "a.resolved")
        with open(self.resolved, "w") as f:
            f.write("resolved\n")
        r = self.repo
        _must(r, "init", "-q", "-b", "main")
        _must(r, "config", "user.email", "t@example.com")
        _must(r, "config", "user.name", "t")
        _must(r, "config", "commit.gpgsign", "false")
        with open(os.path.join(r, "a.txt"), "w") as f:
            f.write("base\n")
        _must(r, "add", "a.txt")
        _must(r, "commit", "-q", "-m", "base")
        _must(r, "checkout", "-q", "-b", "side")
        with open(os.path.join(r, "a.txt"), "w") as f:
            f.write("side\n")
        _must(r, "commit", "-q", "-am", "side")
        _must(r, "checkout", "-q", "main")
        with open(os.path.join(r, "a.txt"), "w") as f:
            f.write("main\n")
        _must(r, "commit", "-q", "-am", "main")

    def tearDown(self) -> None:
        self._tmp.cleanup()

    def test_control_cp_into_main_outside_a_merge_is_refused(self) -> None:
        p = _guard(self.repo, f"cp {self.resolved} a.txt")
        self.assertEqual(p.returncode, 2, f"guard must be live: {p.stderr}")

    @unittest.expectedFailure  # backlog ee273b5e: open defect
    def test_cp_resolved_file_into_conflicted_path_mid_merge_is_allowed(self) -> None:
        m = _git(self.repo, "merge", "side")
        self.assertNotEqual(m.returncode, 0, "precondition: the merge must conflict")
        self.assertTrue(
            os.path.exists(os.path.join(self.repo, ".git", "MERGE_HEAD")),
            "precondition: a merge is in progress",
        )
        u = _git(self.repo, "diff", "--name-only", "--diff-filter=U")
        self.assertIn("a.txt", u.stdout, "precondition: a.txt is conflicted")
        p = _guard(self.repo, f"cp {self.resolved} a.txt")
        self.assertEqual(
            p.returncode,
            0,
            "section 8 permits conflict resolution on main, but the guard refused "
            f"placing the resolved file: rc={p.returncode} stderr={p.stderr!r}",
        )


if __name__ == "__main__":
    unittest.main()
