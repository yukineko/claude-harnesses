"""Verifier round 2 for backlog e033c406 (on 55769b61). Written by the condukt
verifier, not the implementer.

Pins the brick bound: a corrupt ledger older than 20 minutes is moved aside,
and (since e2399b08, by the coordinator's instruction) its valid deny lines are
salvaged into a fresh active ledger, so Stop still blocks on a
refused-then-changed target.

User ruling 2026-10-04: the Bash side of e033c406 is dropped with the observing
guard-maintree-bash.py. The git-pathspec-into-hooks Bash tests were removed;
the denies below are created through guard-maintree-edit.py.

Hooks run as subprocesses against a throwaway repo; HOME and TMPDIR point at a
temp dir, so the real ~/.claude/state and the shared temp dir are not touched.

    python3 -m unittest scripts/test_deny_ledger_e033c406_verify2.py
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
EDIT = os.path.join(HERE, "guard-maintree-edit.py")
STOP = os.path.join(HERE, "stop-verify-worktree.py")


def _git(cwd, *args):
    subprocess.run(("git", "-c", "user.email=v@v", "-c", "user.name=v") + args,
                   cwd=cwd, check=True, capture_output=True)


class _Fixture(unittest.TestCase):
    def setUp(self):
        self.tmp = os.path.realpath(tempfile.mkdtemp(prefix="e033c406v2-"))
        self.addCleanup(shutil.rmtree, self.tmp, True)
        self.main = os.path.join(self.tmp, "main")
        self.wt = os.path.join(self.tmp, "wt")
        self.home = os.path.join(self.tmp, "home")
        self.tmpd = os.path.join(self.tmp, "t")
        for d in (os.path.join(self.main, "src"), os.path.join(self.main, ".githooks"),
                  self.home, self.tmpd):
            os.makedirs(d)
        with open(os.path.join(self.main, "src", "f.txt"), "w") as f:
            f.write("a\n")
        with open(os.path.join(self.main, ".githooks", "pre-commit"), "w") as f:
            f.write("#!/bin/sh\n")
        _git(self.main, "init", "-q")
        _git(self.main, "add", "-A")
        _git(self.main, "commit", "-qm", "init")
        _git(self.main, "worktree", "add", "-q", self.wt, "-b", "wt")
        self.target = os.path.join(self.main, "src", "f.txt")

    def run_hook(self, script, payload, proj=None):
        env = {k: v for k, v in os.environ.items()
               if k not in ("CLAUDECODE", "CLAUDE_CODE_ENTRYPOINT")}
        env.update(HOME=self.home, TMPDIR=self.tmpd,
                   CLAUDE_PROJECT_DIR=proj or self.main)
        return subprocess.run([sys.executable, script], input=json.dumps(payload),
                              text=True, capture_output=True, env=env,
                              cwd=self.main, timeout=60)

    def write(self, path, content="x", sid="v2"):
        return self.run_hook(EDIT, {"session_id": sid, "tool_name": "Write",
                                    "tool_input": {"file_path": path, "content": content}})


class CorruptBound(_Fixture):
    def _ledger(self, sid="v2"):
        return os.path.join(self.home, ".claude", "state", "maintree-deny", sid + ".jsonl")

    def _unrelated(self):
        return self.write(os.path.join(self.wt, "n.md"))

    def test_fresh_corrupt_ledger_refuses(self):
        self.assertEqual(self.write(self.target).returncode, 2)
        with open(self._ledger(), "a") as f:
            f.write("garbage\n")
        self.assertEqual(self._unrelated().returncode, 2)

    def test_stale_corrupt_ledger_is_moved_aside_and_its_denies_salvaged(self):
        self.assertEqual(self.write(self.target).returncode, 2)
        with open(self._ledger(), "a") as f:
            f.write("garbage\n")
        old = time.time() - 21 * 60
        os.utime(self._ledger(), (old, old))
        with open(self.target, "a") as f:
            f.write("changed\n")
        r = self._unrelated()
        self.assertEqual(r.returncode, 0, r.stderr)
        d = os.path.dirname(self._ledger())
        self.assertTrue(any(".corrupt-" in n for n in os.listdir(d)), os.listdir(d))
        # the valid deny survives in a fresh active ledger …
        with open(self._ledger()) as f:
            entries = [json.loads(line) for line in f if line.strip()]
        denies = [e for e in entries if e.get("kind") == "deny"]
        self.assertEqual([e["target_abs"] for e in denies], [self.target])
        # … so Stop still blocks on the refused-then-changed target.
        r = self.run_hook(STOP, {"session_id": "v2", "hook_event_name": "Stop", "cwd": self.wt})
        self.assertEqual(r.returncode, 2, r.stderr)

    def test_fallback_ledger_used_when_home_unwritable(self):
        os.chmod(self.home, 0o500)
        self.addCleanup(os.chmod, self.home, 0o700)
        self.assertEqual(self.write(self.target).returncode, 2)
        self.assertEqual(self.write(os.path.join(self.wt, "n.md"),
                                    content=f"see {self.target}").returncode, 2)


if __name__ == "__main__":
    unittest.main()
