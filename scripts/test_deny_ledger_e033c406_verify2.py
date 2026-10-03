"""Verifier round 2 for backlog e033c406 (on 55769b61). Written by the condukt
verifier, not the implementer.

Pins two bypasses of the hook-machinery gate in classes its docstring claims to
handle ("git subcommands that write working-tree paths named by a pathspec …
when a pathspec … as a git glob (whose `*` crosses `/` and leading dots) can
match `.githooks`"):

  * a git glob pathspec that matches a FILE under .githooks without matching
    the stand-in `.githooks/x` (`'*pre-commit'`): observed with real git to
    restore an old `.githooks/pre-commit` (checkout) or delete it (rm);
  * `-p` treated as an option that takes a value, so it swallows the pathspec
    of `git restore -s OLD -p .githooks` / `git stash push -p .githooks`
    (observed with real git: restore -p with "y" on stdin rewrites the hook).

Also pins the brick bound: a corrupt ledger older than 20 minutes is moved
aside, and (since e2399b08, by the coordinator's instruction) its valid deny
lines are salvaged into a fresh active ledger, so Stop still blocks on a
refused-then-changed target.

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
BASH = os.path.join(HERE, "guard-maintree-bash.py")
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

    def bash(self, cmd, sid="v2", cwd=None, proj=None):
        return self.run_hook(BASH, {"session_id": sid, "tool_name": "Bash",
                                    "tool_input": {"command": cmd},
                                    "cwd": cwd or self.main}, proj=proj)


class GitPathspecIntoHooks(_Fixture):
    def _refused_everywhere(self, cmd):
        for cwd in (self.main, self.wt):
            with self.subTest(cwd=cwd):
                r = self.bash(cmd, cwd=cwd, proj=cwd)
                self.assertEqual(r.returncode, 2, r.stderr)

    def test_named_pathspecs_refused(self):
        # already closed by 55769b61: guards against regression
        for cmd in ("git checkout HEAD~1 -- .githooks",
                    "git restore -s HEAD~1 .githooks",
                    "git stash push -m x -- .githooks",
                    "git checkout HEAD~1 -- '*/pre-commit'"):
            self._refused_everywhere(cmd)

    def test_glob_matching_a_hook_file_is_refused(self):
        self._refused_everywhere("git checkout HEAD~1 -- '*pre-commit'")

    def test_glob_rm_matching_a_hook_file_is_refused(self):
        self._refused_everywhere("git rm '*pre-commit'")

    def test_restore_patch_mode_pathspec_is_refused(self):
        self._refused_everywhere("git restore -s HEAD~1 -p .githooks")

    def test_stash_push_patch_mode_pathspec_is_refused(self):
        self._refused_everywhere("git stash push -p .githooks")

    def test_ordinary_pathspecs_unaffected(self):
        for cmd in ("git checkout -- src/f.txt", "git restore .", "git stash push -m wip",
                    "git rm '*.orig'", "git checkout HEAD~1 -- '*.rs'", "git apply -p1 x.patch"):
            with self.subTest(cmd=cmd):
                self.assertEqual(self.bash(cmd, cwd=self.wt, proj=self.wt).returncode, 0)


class CorruptBound(_Fixture):
    def _ledger(self, sid="v2"):
        return os.path.join(self.home, ".claude", "state", "maintree-deny", sid + ".jsonl")

    def test_fresh_corrupt_ledger_refuses(self):
        self.assertEqual(self.bash(f"echo x > {self.target}").returncode, 2)
        with open(self._ledger(), "a") as f:
            f.write("garbage\n")
        self.assertEqual(self.bash("ls").returncode, 2)

    def test_stale_corrupt_ledger_is_moved_aside_and_its_denies_salvaged(self):
        self.assertEqual(self.bash(f"echo x > {self.target}").returncode, 2)
        with open(self._ledger(), "a") as f:
            f.write("garbage\n")
        old = time.time() - 21 * 60
        os.utime(self._ledger(), (old, old))
        with open(self.target, "a") as f:
            f.write("changed\n")
        r = self.bash("ls")
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
        self.assertEqual(self.bash(f"echo x > {self.target}").returncode, 2)
        self.assertEqual(self.bash(f"cat {self.target}").returncode, 2)


if __name__ == "__main__":
    unittest.main()
