#!/usr/bin/env python3
"""Tests for the maintree deny ledger and the hook-machinery gate (e033c406).

Drives the REAL hook scripts as subprocesses (guard-maintree-bash.py,
guard-maintree-edit.py, stop-verify-worktree.py, deny-ledger-clear.py) in a
throwaway repo + linked worktree, with HOME pointed at a temp dir so the
per-session ledger (`$HOME/.claude/state/maintree-deny/<sid>.jsonl`) never
touches the developer's real one. The interactive/non-interactive split is
pinned explicitly through CLAUDECODE / CLAUDE_CODE_ENTRYPOINT.

    python3 -m unittest scripts.test_deny_ledger_e033c406
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

SCRIPTS = os.path.dirname(os.path.abspath(__file__))
BASH = os.path.join(SCRIPTS, "guard-maintree-bash.py")
EDIT = os.path.join(SCRIPTS, "guard-maintree-edit.py")
STOP = os.path.join(SCRIPTS, "stop-verify-worktree.py")
CLEAR = os.path.join(SCRIPTS, "deny-ledger-clear.py")
SID = "e033c406-ledger-session"

GIT_ENV = {
    "GIT_CONFIG_GLOBAL": os.devnull, "GIT_CONFIG_NOSYSTEM": "1",
    "GIT_AUTHOR_NAME": "t", "GIT_AUTHOR_EMAIL": "t@t",
    "GIT_COMMITTER_NAME": "t", "GIT_COMMITTER_EMAIL": "t@t",
}


class _Fixture(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = os.path.realpath(tempfile.mkdtemp(prefix="e033c406-ledger."))
        self.addCleanup(shutil.rmtree, self.tmp, True)
        self.home = os.path.join(self.tmp, "home")
        os.makedirs(self.home)
        self.main = os.path.join(self.tmp, "main")
        os.makedirs(self.main)
        self.git("init", "-q", "-b", "main", self.main, cwd=self.tmp)
        os.makedirs(os.path.join(self.main, ".githooks"))
        for name, text in (("tracked.rs", "hi\n"), ("other.rs", "o\n"),
                           (".githooks/pre-commit", "#!/bin/sh\nexit 0\n")):
            with open(os.path.join(self.main, name), "w") as f:
                f.write(text)
        self.git("add", "-A")
        self.git("commit", "-qm", "init")
        self.wt = os.path.join(self.tmp, "wtA")
        self.git("worktree", "add", "-q", self.wt, "-b", "feat", "HEAD")
        self.target = os.path.join(self.main, "tracked.rs")
        self.ledger = os.path.join(self.home, ".claude", "state", "maintree-deny",
                                   SID + ".jsonl")

    def git(self, *args: str, cwd: str | None = None) -> None:
        subprocess.run(["git", *args], cwd=cwd or self.main, check=True,
                       env=dict(os.environ, **GIT_ENV), capture_output=True)

    def env(self, interactive: bool, project: str | None = None) -> dict:
        env = dict(os.environ, **GIT_ENV)
        env["CLAUDE_PROJECT_DIR"] = project or self.main
        env["HOME"] = self.home
        env["CLAUDECODE"] = "1"
        env["CLAUDE_CODE_ENTRYPOINT"] = "cli" if interactive else "sdk-cli"
        return env

    def run_hook(self, script: str, payload: dict, interactive: bool = False,
                 project: str | None = None, cwd: str | None = None):
        return subprocess.run(
            [sys.executable, script], cwd=cwd or self.main,
            input=json.dumps(payload), capture_output=True, text=True,
            env=self.env(interactive, project), timeout=60,
        )

    def bash(self, cmd: str, sid: str | None = SID, cwd: str | None = None,
             interactive: bool = False, project: str | None = None):
        payload = {"hook_event_name": "PreToolUse", "cwd": cwd or self.main,
                   "tool_name": "Bash", "tool_input": {"command": cmd}}
        if sid is not None:
            payload["session_id"] = sid
        return self.run_hook(BASH, payload, interactive, project, cwd)

    def edit(self, path: str, sid: str = SID, interactive: bool = False,
             project: str | None = None):
        payload = {"hook_event_name": "PreToolUse", "cwd": self.main,
                   "session_id": sid, "tool_name": "Write",
                   "tool_input": {"file_path": path, "content": "x\n"}}
        return self.run_hook(EDIT, payload, interactive, project)

    def stop(self, sid: str = SID, cwd: str | None = None, active: bool = False):
        payload = {"hook_event_name": "Stop", "session_id": sid,
                   "cwd": cwd or self.wt, "stop_hook_active": active}
        return self.run_hook(STOP, payload, cwd=cwd or self.wt)

    def clear(self, sid: str = SID):
        payload = {"hook_event_name": "UserPromptSubmit", "session_id": sid,
                   "cwd": self.main, "prompt": "next instruction"}
        return self.run_hook(CLEAR, payload)

    def deny_rm(self, sid: str = SID):
        r = self.bash("rm " + self.target, sid=sid)
        self.assertEqual(r.returncode, 2, "control: rm of a main file is denied")
        return r

    def entries(self) -> list[dict]:
        with open(self.ledger) as f:
            return [json.loads(l) for l in f if l.strip()]


class DenyIsRecorded(_Fixture):
    def test_bash_deny_writes_a_ledger_entry_under_home(self):
        self.deny_rm()
        denies = [e for e in self.entries() if e["kind"] == "deny"]
        self.assertEqual(len(denies), 1)
        e = denies[0]
        self.assertEqual(e["target_abs"], self.target)
        self.assertEqual(e["root"], self.main)
        self.assertEqual(e["denier"], "guard-maintree-bash.py")
        self.assertIn("rm " + self.target, e["raw"])
        self.assertTrue(e["reason"])
        self.assertTrue(e["snapshot"]["exists"])
        self.assertEqual(e["snapshot"]["status"], "")
        # Never in the project root (CLAUDE.md §5).
        self.assertFalse(os.path.exists(os.path.join(self.main, ".claude")))

    def test_allowed_calls_with_no_ledger_write_nothing(self):
        r = self.bash("ls " + self.main)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertFalse(os.path.exists(self.ledger))


class RetryIsRefused(_Fixture):
    def test_retry_naming_the_denied_target_is_refused_quoting_the_deny(self):
        self.deny_rm()
        # `cat` is allowed by the guard's own rules; only the ledger refuses it.
        retry = self.bash("cat " + self.target)
        self.assertEqual(retry.returncode, 2, retry.stderr)
        self.assertIn("guard-maintree-bash.py refused `rm " + self.target + "`",
                      retry.stderr)
        self.assertIn("This call reaches the same target " + self.target +
                      " by another spelling; confirm it is genuinely a "
                      "different approach.", retry.stderr)

    def test_retry_is_an_ask_in_an_interactive_session(self):
        self.deny_rm()
        retry = self.bash("cat " + self.target, interactive=True)
        self.assertEqual(retry.returncode, 0, retry.stderr)
        out = json.loads(retry.stdout)["hookSpecificOutput"]
        self.assertEqual(out["permissionDecision"], "ask")
        self.assertEqual(out["hookEventName"], "PreToolUse")
        self.assertIn("refused `rm " + self.target + "`",
                      out["permissionDecisionReason"])

    def test_retry_through_the_edit_tool_is_refused(self):
        self.deny_rm()
        # The worktree-path edit is allowed by the edit guard's own rules, but
        # a call naming the denied main path is not.
        r = self.edit(self.target)
        self.assertEqual(r.returncode, 2)
        r2 = self.run_hook(EDIT, {
            "session_id": SID, "tool_name": "Edit", "cwd": self.wt,
            "tool_input": {"file_path": os.path.join(self.wt, "tracked.rs"),
                           "old_string": "see " + self.target, "new_string": "x"}})
        self.assertEqual(r2.returncode, 2, r2.stderr)
        self.assertIn("by another spelling", r2.stderr)

    def test_edit_deny_then_bash_retry_is_refused(self):
        r = self.edit(self.target)
        self.assertEqual(r.returncode, 2, "control: Write into main is denied")
        retry = self.bash("cat " + self.target)
        self.assertEqual(retry.returncode, 2, retry.stderr)
        self.assertIn("guard-maintree-edit.py refused", retry.stderr)

    def test_worktree_redo_is_allowed(self):
        self.deny_rm()
        redo = self.bash("rm " + os.path.join(self.wt, "tracked.rs"), cwd=self.wt)
        self.assertEqual(redo.returncode, 0, redo.stderr)
        self.assertEqual(redo.stdout, "")
        redo_edit = self.edit(os.path.join(self.wt, "tracked.rs"))
        self.assertEqual(redo_edit.returncode, 0, redo_edit.stderr)

    def test_a_longer_name_sharing_the_prefix_is_not_the_target(self):
        self.deny_rm()
        r = self.bash("cat " + self.target + ".orig " + self.target + "2")
        self.assertEqual(r.returncode, 0, r.stderr)

    def test_a_different_session_is_unaffected(self):
        self.deny_rm()
        r = self.bash("cat " + self.target, sid="another-session")
        self.assertEqual(r.returncode, 0, r.stderr)

    def test_the_window_expires_by_time(self):
        self.deny_rm()
        lines = self.entries()
        for e in lines:
            e["epoch"] -= 3600
        with open(self.ledger, "w") as f:
            f.write("".join(json.dumps(e) + "\n" for e in lines))
        r = self.bash("cat " + self.target)
        self.assertEqual(r.returncode, 0, r.stderr)

    def test_the_window_expires_by_call_count(self):
        self.deny_rm()
        with open(self.ledger, "a") as f:
            for _ in range(25):
                f.write(json.dumps({"v": 1, "kind": "tick", "epoch": time.time()}) + "\n")
        r = self.bash("cat " + self.target)
        self.assertEqual(r.returncode, 0, r.stderr)

    def test_calls_inside_the_window_are_counted(self):
        self.deny_rm()
        self.assertEqual(self.bash("ls /").returncode, 0)
        self.assertEqual([e["kind"] for e in self.entries()], ["deny", "tick"])


class UndeterminedLedgerRefuses(_Fixture):
    def _corrupt(self, text: str) -> None:
        os.makedirs(os.path.dirname(self.ledger), exist_ok=True)
        with open(self.ledger, "w") as f:
            f.write(text)

    def test_corrupt_ledger_refuses_an_unrelated_call(self):
        self._corrupt("{this is not json\n")
        r = self.bash("ls /")
        self.assertEqual(r.returncode, 2, r.stderr)
        self.assertIn("could not be consulted", r.stderr)
        e = self.edit(os.path.join(self.wt, "tracked.rs"))
        self.assertEqual(e.returncode, 2, e.stderr)

    def test_corrupt_ledger_asks_in_an_interactive_session(self):
        self._corrupt('{"v": 1, "kind": "deny"}\n')  # missing fields
        r = self.bash("ls /", interactive=True)
        self.assertEqual(r.returncode, 0)
        self.assertEqual(json.loads(r.stdout)["hookSpecificOutput"]
                         ["permissionDecision"], "ask")

    def test_unreadable_ledger_refuses(self):
        os.makedirs(self.ledger)  # a directory where the file should be
        r = self.bash("ls /")
        self.assertEqual(r.returncode, 2, r.stderr)

    def test_unusable_session_id_refuses(self):
        r = self.bash("ls /", sid="../escape")
        self.assertEqual(r.returncode, 2, r.stderr)

    def test_missing_ledger_is_no_prior_denies(self):
        r = self.bash("ls /")
        self.assertEqual(r.returncode, 0, r.stderr)

    def test_corrupt_ledger_blocks_the_stop(self):
        self._corrupt("garbage\n")
        r = self.stop()
        self.assertEqual(r.returncode, 2, r.stderr)

    def test_edit_guard_refuses_an_unreadable_payload(self):
        r = subprocess.run([sys.executable, EDIT], input="not json {",
                           capture_output=True, text=True, env=self.env(False))
        self.assertEqual(r.returncode, 2)


class UserPromptSubmitClears(_Fixture):
    def test_clear_removes_the_session_ledger_and_lifts_the_refusal(self):
        self.deny_rm()
        self.assertTrue(os.path.exists(self.ledger))
        r = self.clear()
        self.assertEqual(r.returncode, 0)
        self.assertEqual(r.stdout, "")
        self.assertFalse(os.path.exists(self.ledger))
        self.assertEqual(self.bash("cat " + self.target).returncode, 0)

    def test_clear_leaves_other_sessions_alone(self):
        self.deny_rm(sid="other-session")
        self.clear()
        other = os.path.join(os.path.dirname(self.ledger), "other-session.jsonl")
        self.assertTrue(os.path.exists(other))


class StopDetectsChange(_Fixture):
    def test_stop_blocks_when_a_denied_path_changed(self):
        self.deny_rm()
        os.remove(self.target)  # the refused effect, by a route no guard saw
        r = self.stop()
        self.assertEqual(r.returncode, 2, r.stderr)
        self.assertIn("guard-maintree-bash.py refused `rm " + self.target + "`",
                      r.stderr)

    def test_stop_blocks_when_a_denied_path_was_rewritten(self):
        self.deny_rm()
        with open(self.target, "w") as f:
            f.write("changed\n")
        self.assertEqual(self.stop().returncode, 2)

    def test_stop_allows_when_the_denied_path_is_unchanged(self):
        self.deny_rm()
        r = self.stop()
        self.assertEqual(r.returncode, 0, r.stderr)

    def test_stop_allows_a_change_that_landed_as_a_commit(self):
        # A merge on main moves HEAD and leaves the path clean: not this gate's
        # call (check-worktree-isolation.py judges how HEAD moved).
        self.deny_rm()
        with open(self.target, "w") as f:
            f.write("merged\n")
        self.git("commit", "-qam", "land")
        self.assertEqual(self.stop().returncode, 0)

    def test_stop_hook_active_is_the_bounded_allow(self):
        self.deny_rm()
        os.remove(self.target)
        self.assertEqual(self.stop(active=True).returncode, 0)

    def test_other_session_stop_is_unaffected(self):
        self.deny_rm()
        os.remove(self.target)
        self.assertEqual(self.stop(sid="another-session").returncode, 0)


class HookMachineryIsProtected(_Fixture):
    def test_hookspath_rewiring_spellings_are_refused(self):
        for cmd in (
            "git config core.hooksPath /dev/null",
            "git config --local core.hooksPath /tmp/x",
            "git config --global core.HooksPath ''",
            "git config set core.hooksPath x",
            "git config --unset core.hooksPath",
            "git config unset core.hooksPath",
            "git config --remove-section core",
            "git config --edit",
            "git -c core.hooksPath=/dev/null commit -m x",
            "git -C " + self.main + " config core.hooksPath /dev/null",
            "GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=core.hooksPath "
            "GIT_CONFIG_VALUE_0=/dev/null git commit -m x",
            "sh -c 'git config core.hooksPath /dev/null'",
        ):
            with self.subTest(cmd=cmd):
                r = self.bash(cmd, cwd=self.wt)
                self.assertEqual(r.returncode, 2, r.stderr)

    def test_hook_dir_writes_are_refused_from_a_worktree(self):
        for cmd in (
            "chmod -x .githooks/pre-commit",
            "rm -rf .githooks",
            "echo exit 0 > .githooks/pre-commit",
            "cp /dev/null " + os.path.join(self.main, ".git", "hooks", "pre-commit"),
            "echo x >> " + os.path.join(self.main, ".git", "config"),
            "cd .githooks && rm pre-commit",
        ):
            with self.subTest(cmd=cmd):
                r = self.bash(cmd, cwd=self.wt)
                self.assertEqual(r.returncode, 2, r.stderr)

    def test_hooks_are_protected_when_the_project_anchor_is_a_worktree(self):
        for cmd in ("git config core.hooksPath /dev/null",
                    "chmod -x " + os.path.join(self.wt, ".githooks", "pre-commit")):
            with self.subTest(cmd=cmd):
                r = self.bash(cmd, cwd=self.wt, project=self.wt)
                self.assertEqual(r.returncode, 2, r.stderr)
        ok = self.bash("rm " + os.path.join(self.wt, "tracked.rs"),
                       cwd=self.wt, project=self.wt)
        self.assertEqual(ok.returncode, 0, ok.stderr)

    def test_reading_and_the_sanctioned_setting_are_allowed(self):
        for cmd in (
            "git config core.hooksPath",
            "git config --get core.hooksPath",
            "git config get core.hooksPath",
            "git config core.hooksPath .githooks",
            "cat .githooks/pre-commit",
            "ls -la .git/hooks",
            "grep -rn hooksPath docs",
            "git -c core.hooksPath=.githooks status",
        ):
            with self.subTest(cmd=cmd):
                r = self.bash(cmd, cwd=self.wt)
                self.assertEqual(r.returncode, 0, r.stderr)

    def test_edit_tool_into_hook_machinery_is_refused_in_any_tree(self):
        for path in (os.path.join(self.wt, ".githooks", "pre-commit"),
                     os.path.join(self.main, ".git", "hooks", "pre-commit"),
                     os.path.join(self.main, ".git", "config")):
            with self.subTest(path=path):
                r = self.edit(path, project=self.wt)
                self.assertEqual(r.returncode, 2, r.stderr)
                self.assertIn("hook machinery", r.stderr)


if __name__ == "__main__":
    unittest.main()
