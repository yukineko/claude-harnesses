"""Independent verifier tests for backlog e033c406 (maintree deny ledger),
Edit/Write side. Written by the condukt verifier, not the implementer.

Every hook is run as a subprocess against a throwaway repo (a main checkout and
one linked worktree) with HOME pointed at a temp dir, so the real
~/.claude/state is never touched.

User ruling 2026-10-04: the Bash side of e033c406 (Bash refusal recording, the
Bash retry gate, the Bash hook-machinery gate) is dropped with the observing
guard-maintree-bash.py. The denies below are created through
guard-maintree-edit.py; the hook-machinery Bash tests were removed.

    python3 -m unittest scripts/test_deny_ledger_e033c406_verify.py
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
EDIT = os.path.join(HERE, "guard-maintree-edit.py")
STOP = os.path.join(HERE, "stop-verify-worktree.py")
CLEAR = os.path.join(HERE, "deny-ledger-clear.py")


def _git(cwd, *args):
    subprocess.run(("git", "-c", "user.email=v@v", "-c", "user.name=v") + args,
                   cwd=cwd, check=True, capture_output=True)


class _Fixture(unittest.TestCase):
    def setUp(self):
        self.tmp = os.path.realpath(tempfile.mkdtemp(prefix="e033c406v-"))
        self.addCleanup(shutil.rmtree, self.tmp, True)
        self.main = os.path.join(self.tmp, "main")
        self.wt = os.path.join(self.tmp, "wt")
        self.home = os.path.join(self.tmp, "home")
        os.makedirs(os.path.join(self.main, "src"))
        os.makedirs(os.path.join(self.main, ".githooks"))
        os.makedirs(self.home)
        with open(os.path.join(self.main, "src", "f.txt"), "w") as f:
            f.write("a\n")
        with open(os.path.join(self.main, ".githooks", "pre-commit"), "w") as f:
            f.write("#!/bin/sh\n")
        _git(self.main, "init", "-q")
        _git(self.main, "add", "-A")
        _git(self.main, "commit", "-qm", "init")
        _git(self.main, "worktree", "add", "-q", self.wt, "-b", "wt")
        self.target = os.path.join(self.main, "src", "f.txt")

    def run_hook(self, script, payload, interactive=False, proj=None):
        env = {k: v for k, v in os.environ.items()
               if k not in ("CLAUDECODE", "CLAUDE_CODE_ENTRYPOINT")}
        env["HOME"] = self.home
        env["CLAUDE_PROJECT_DIR"] = proj or self.main
        if interactive:
            env.update(CLAUDECODE="1", CLAUDE_CODE_ENTRYPOINT="cli")
        data = payload if isinstance(payload, str) else json.dumps(payload)
        return subprocess.run([sys.executable, script], input=data, text=True,
                              capture_output=True, env=env, cwd=self.main,
                              timeout=60)

    def write(self, path, content="x", sid="s1", **kw):
        return self.run_hook(EDIT, {"session_id": sid, "tool_name": "Write",
                                    "tool_input": {"file_path": path,
                                                   "content": content}}, **kw)

    def note(self, text, sid="s1", **kw):
        """A Write to a worktree file (allowed by the edit guard's own rules)
        whose content names `text`."""
        return self.write(os.path.join(self.wt, "n.md"), f"see {text}", sid=sid, **kw)

    def stop(self, sid="s1", **kw):
        return self.run_hook(STOP, {"session_id": sid, "hook_event_name": "Stop",
                                    "cwd": self.wt}, **kw)

    def ledger(self, sid="s1"):
        return os.path.join(self.home, ".claude", "state", "maintree-deny", sid + ".jsonl")


class LedgerRecording(_Fixture):
    def test_deny_is_recorded_under_home_not_project(self):
        r = self.write(self.target)
        self.assertEqual(r.returncode, 2, r.stderr)
        with open(self.ledger()) as f:
            entries = [json.loads(line) for line in f if line.strip()]
        deny = [e for e in entries if e.get("kind") == "deny"]
        self.assertEqual(len(deny), 1, entries)
        for key in ("ts", "session_id", "denier", "target_abs", "raw", "reason", "snapshot"):
            self.assertIn(key, deny[0])
        self.assertEqual(deny[0]["target_abs"], self.target)
        self.assertEqual(deny[0]["session_id"], "s1")
        for root, _dirs, files in os.walk(self.main):
            self.assertFalse(any(f.endswith(".jsonl") for f in files), root)


class RetryPrevention(_Fixture):
    def setUp(self):
        super().setUp()
        self.assertEqual(self.write(self.target).returncode, 2)

    def test_retry_naming_target_is_denied_noninteractive_quoting_deny(self):
        r = self.note(self.target)
        self.assertEqual(r.returncode, 2, r.stderr)
        self.assertIn("guard-maintree-edit.py refused", r.stderr)
        self.assertIn(f"Write {self.target}", r.stderr)

    def test_retry_is_ask_when_interactive(self):
        r = self.note(self.target, interactive=True)
        self.assertEqual(r.returncode, 0, r.stderr)
        out = json.loads(r.stdout)
        self.assertEqual(out["hookSpecificOutput"]["permissionDecision"], "ask")

    def test_worktree_redo_is_silent(self):
        r = self.write(os.path.join(self.wt, "src", "f.txt"))
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout, "")

    def test_other_session_is_unaffected(self):
        self.assertEqual(self.note(self.target, sid="s2").returncode, 0)

    def test_sibling_name_is_not_matched(self):
        self.assertEqual(self.note(f"{self.target}2").returncode, 0)

    def test_window_expires_after_25_guarded_calls(self):
        for _ in range(25):
            self.write(os.path.join(self.wt, "other.md"))
        self.assertEqual(self.note(self.target).returncode, 0)

    def test_user_prompt_clears_ledger(self):
        r = self.run_hook(CLEAR, {"session_id": "s1", "hook_event_name": "UserPromptSubmit"})
        self.assertEqual(r.returncode, 0)
        self.assertFalse(os.path.exists(self.ledger()))
        self.assertEqual(self.note(self.target).returncode, 0)


class LedgerUndetermined(_Fixture):
    def test_missing_ledger_means_no_denies(self):
        self.assertEqual(self.write(os.path.join(self.wt, "x")).returncode, 0)
        self.assertEqual(self.stop().returncode, 0)

    def test_corrupt_ledger_refuses_edit_and_stop(self):
        os.makedirs(os.path.dirname(self.ledger()))
        with open(self.ledger(), "w") as f:
            f.write('{"v": 1, "kind": "ti')
        r = self.write(os.path.join(self.wt, "x"), content="")
        self.assertEqual(r.returncode, 2)
        self.assertEqual(self.stop().returncode, 2)

    def test_unusable_session_id_refuses(self):
        self.assertEqual(self.write(os.path.join(self.wt, "x"), sid="../evil").returncode, 2)

    def test_edit_guard_refuses_unparseable_payload(self):
        self.assertEqual(self.run_hook(EDIT, "not json").returncode, 2)


class StopDetection(_Fixture):
    def test_change_through_git_route_blocks_stop(self):
        self.assertEqual(self.write(self.target).returncode, 2)
        self.assertEqual(self.stop().returncode, 0)
        with open(os.path.join(self.wt, "src", "f.txt"), "w") as f:
            f.write("changed\n")
        _git(self.wt, "commit", "-qam", "c")
        # a git subcommand the edit guard never sees
        _git(self.main, "checkout", "wt", "--", "src/f.txt")
        r = self.stop()
        self.assertEqual(r.returncode, 2, r.stderr)
        self.assertIn("REFUSED", r.stderr)


if __name__ == "__main__":
    unittest.main()
