#!/usr/bin/env python3
"""session-worktree-init.py must register the worktree it creates (backlog 491f6e94).

USER RULING 2026-09-30 (option A): at creation the hook writes a `*.driver`
record for the worktree path, in the layout condukt's `registrations_in` reads:
`$HOME/.backlog/drivers/<bucket>/<name>.driver`, JSON with `project` (the
worktree path) and `heartbeat_at`. Without it every session worktree stays
Undetermined forever and nothing can ever reclaim it.

Run: python3 scripts/test_session_worktree_init_registers.py
"""
import json
import os
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path

HOOK = Path(__file__).resolve().parent / "session-worktree-init.py"


def git(cwd, *args):
    subprocess.run(("git", *args), cwd=cwd, check=True, capture_output=True)


def drivers(home):
    root = Path(home) / ".backlog" / "drivers"
    return list(root.glob("*/*.driver")) if root.is_dir() else []


class Fixture:
    def __init__(self):
        self.tmp = tempfile.TemporaryDirectory()
        base = Path(os.path.realpath(self.tmp.name))
        self.home = base / "home"
        self.home.mkdir()
        self.repo = base / "repo"
        self.repo.mkdir()
        git(self.repo, "init", "-q", "-b", "main")
        git(self.repo, "config", "user.email", "t@t.t")
        git(self.repo, "config", "user.name", "t")
        (self.repo / "a.txt").write_text("a\n")
        git(self.repo, "add", "a.txt")
        git(self.repo, "commit", "-q", "-m", "seed")

    def run_hook(self, cwd, sid):
        env = dict(os.environ, HOME=str(self.home))
        return subprocess.run(
            (sys.executable, str(HOOK)),
            input=json.dumps({"cwd": str(cwd), "session_id": sid}),
            capture_output=True, text=True, env=env, timeout=120,
        )


class RegistersWorktree(unittest.TestCase):
    def test_creation_writes_a_driver_registration_for_the_worktree(self):
        f = Fixture()
        out = f.run_hook(f.repo, "abcd1234-0000")
        self.assertEqual(out.returncode, 0, out.stderr)
        wt = f.repo.parent / ".repo-worktrees" / "session-abcd1234"
        self.assertTrue(wt.is_dir(), f"worktree not created: {out.stdout}")
        recs = drivers(f.home)
        self.assertTrue(recs, f"no *.driver record under {f.home}/.backlog/drivers")
        projects = [os.path.realpath(json.loads(r.read_text())["project"]) for r in recs]
        self.assertIn(os.path.realpath(wt), projects)
        rec = next(json.loads(r.read_text()) for r in recs
                   if os.path.realpath(json.loads(r.read_text())["project"]) == os.path.realpath(wt))
        self.assertLess(abs(time.time() - rec["heartbeat_at"]), 300,
                        "heartbeat_at must be 'now' at creation")

    def test_control_starting_inside_a_worktree_registers_nothing(self):
        # Already in a linked worktree: the hook creates nothing, so it has
        # nothing to register (guards against register-everything).
        f = Fixture()
        wt = f.repo.parent / "linked"
        git(f.repo, "worktree", "add", "-q", "-b", "linked", str(wt))
        out = f.run_hook(wt, "ffff0000-1")
        self.assertEqual(out.returncode, 0, out.stderr)
        self.assertEqual(drivers(f.home), [])


if __name__ == "__main__":
    unittest.main()
