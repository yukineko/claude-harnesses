"""Independent verifier tests for backlog e033c406 (maintree deny ledger and
the hook-machinery gate). Written by the condukt verifier, not the implementer.

Every hook is run as a subprocess against a throwaway repo (a main checkout and
one linked worktree) with HOME pointed at a temp dir, so the real
~/.claude/state is never touched.

The three expectedFailure tests pin bypasses of the hook-machinery gate that the
verifier observed on 9a756509 (a shell variable naming core.hooksPath, a
quote-split GIT_CONFIG_KEY_n, and git subcommands deleting `.githooks`).

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
BASH = os.path.join(HERE, "guard-maintree-bash.py")
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

    def bash(self, cmd, sid="s1", cwd=None, **kw):
        return self.run_hook(BASH, {"session_id": sid, "tool_name": "Bash",
                                    "tool_input": {"command": cmd},
                                    "cwd": cwd or self.main}, **kw)

    def stop(self, sid="s1", **kw):
        return self.run_hook(STOP, {"session_id": sid, "hook_event_name": "Stop",
                                    "cwd": self.wt}, **kw)

    def ledger(self, sid="s1"):
        return os.path.join(self.home, ".claude", "state", "maintree-deny", sid + ".jsonl")


class LedgerRecording(_Fixture):
    def test_deny_is_recorded_under_home_not_project(self):
        r = self.bash(f"echo x > {self.target}")
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

    def test_edit_guard_records_too(self):
        r = self.run_hook(EDIT, {"session_id": "s1", "tool_name": "Write",
                                 "tool_input": {"file_path": self.target, "content": "x"}})
        self.assertEqual(r.returncode, 2, r.stderr)
        self.assertTrue(os.path.exists(self.ledger()))


class RetryPrevention(_Fixture):
    def setUp(self):
        super().setUp()
        self.assertEqual(self.bash(f"echo x > {self.target}").returncode, 2)

    def test_retry_naming_target_is_denied_noninteractive_quoting_deny(self):
        r = self.bash(f"cat {self.target}")
        self.assertEqual(r.returncode, 2, r.stderr)
        self.assertIn("guard-maintree-bash.py refused", r.stderr)
        self.assertIn(f"echo x > {self.main}"[:60], r.stderr)

    def test_retry_is_ask_when_interactive(self):
        r = self.bash(f"cat {self.target}", interactive=True)
        self.assertEqual(r.returncode, 0, r.stderr)
        out = json.loads(r.stdout)
        self.assertEqual(out["hookSpecificOutput"]["permissionDecision"], "ask")

    def test_retry_through_edit_tool_on_other_file_mentioning_target(self):
        r = self.run_hook(EDIT, {"session_id": "s1", "tool_name": "Write",
                                 "tool_input": {"file_path": os.path.join(self.wt, "n.md"),
                                                "content": f"see {self.target}"}})
        self.assertEqual(r.returncode, 2, r.stderr)

    def test_worktree_redo_is_silent(self):
        r = self.bash(f"echo x > {os.path.join(self.wt, 'src', 'f.txt')}", cwd=self.wt)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout, "")

    def test_other_session_is_unaffected(self):
        self.assertEqual(self.bash(f"cat {self.target}", sid="s2").returncode, 0)

    def test_sibling_name_is_not_matched(self):
        self.assertEqual(self.bash(f"cat {self.target}2").returncode, 0)

    def test_window_expires_after_25_guarded_calls(self):
        for _ in range(25):
            self.bash("ls")
        self.assertEqual(self.bash(f"cat {self.target}").returncode, 0)

    def test_user_prompt_clears_ledger(self):
        r = self.run_hook(CLEAR, {"session_id": "s1", "hook_event_name": "UserPromptSubmit"})
        self.assertEqual(r.returncode, 0)
        self.assertFalse(os.path.exists(self.ledger()))
        self.assertEqual(self.bash(f"cat {self.target}").returncode, 0)


class LedgerUndetermined(_Fixture):
    def test_missing_ledger_means_no_denies(self):
        self.assertEqual(self.bash("ls").returncode, 0)
        self.assertEqual(self.stop().returncode, 0)

    def test_corrupt_ledger_refuses_bash_edit_and_stop(self):
        os.makedirs(os.path.dirname(self.ledger()))
        with open(self.ledger(), "w") as f:
            f.write('{"v": 1, "kind": "ti')
        self.assertEqual(self.bash("ls").returncode, 2)
        r = self.run_hook(EDIT, {"session_id": "s1", "tool_name": "Write",
                                 "tool_input": {"file_path": os.path.join(self.wt, "x"), "content": ""}})
        self.assertEqual(r.returncode, 2)
        self.assertEqual(self.stop().returncode, 2)

    def test_unusable_session_id_refuses(self):
        self.assertEqual(self.bash("ls", sid="../evil").returncode, 2)

    def test_edit_guard_refuses_unparseable_payload(self):
        self.assertEqual(self.run_hook(EDIT, "not json").returncode, 2)


class StopDetection(_Fixture):
    def test_change_through_git_route_blocks_stop(self):
        self.assertEqual(self.bash(f"echo x > {self.target}").returncode, 2)
        self.assertEqual(self.stop().returncode, 0)
        with open(os.path.join(self.wt, "src", "f.txt"), "w") as f:
            f.write("changed\n")
        _git(self.wt, "commit", "-qam", "c")
        # a git subcommand neither PreToolUse guard judges
        self.assertEqual(self.bash("git checkout wt -- src/f.txt").returncode, 0)
        _git(self.main, "checkout", "wt", "--", "src/f.txt")
        r = self.stop()
        self.assertEqual(r.returncode, 2, r.stderr)
        self.assertIn("REFUSED", r.stderr)


class HookMachinery(_Fixture):
    WRITES = [
        "git config core.hooksPath /dev/null",
        "git config --unset core.hooksPath",
        "git config --file .git/config core.hooksPath x",
        "git -c core.hooksPath=/dev/null commit -m a",
        "GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=core.hooksPath GIT_CONFIG_VALUE_0=/dev/null git commit -m a",
        "sed -i '' s/a/b/ .git/config",
        "ln -sf /tmp/x .git/hooks/pre-commit",
        "cp /tmp/x .git/hooks/pre-commit",
        "rm .githooks/pre-commit",
        "chmod -x .githooks/pre-commit",
        "python3 -c \"open('.githooks/pre-commit','w')\"",
    ]
    READS = ["git config core.hooksPath", "git config --get core.hooksPath",
             "cat .githooks/pre-commit", "ls .git/hooks", "cat .git/config"]

    def test_rewiring_refused_from_main_and_worktree(self):
        for cwd in (self.main, self.wt):
            for cmd in self.WRITES:
                with self.subTest(cwd=cwd, cmd=cmd):
                    r = self.bash(cmd, cwd=cwd, sid="h", proj=cwd)
                    self.assertEqual(r.returncode, 2, r.stderr)

    def test_reads_allowed(self):
        for cwd in (self.main, self.wt):
            for cmd in self.READS:
                with self.subTest(cwd=cwd, cmd=cmd):
                    self.assertEqual(self.bash(cmd, cwd=cwd, sid="h", proj=cwd).returncode, 0)

    @unittest.expectedFailure  # verifier finding on 9a756509: variable-held key
    def test_variable_held_key_is_refused(self):
        r = self.bash('k=core.hooksPath; git config "$k" /dev/null', cwd=self.wt, proj=self.wt)
        self.assertEqual(r.returncode, 2, r.stderr)

    @unittest.expectedFailure  # verifier finding on 9a756509: quote-split env key
    def test_quote_split_env_key_is_refused(self):
        r = self.bash('GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=core.hooks""Path '
                      'GIT_CONFIG_VALUE_0=/dev/null git commit -m a')
        self.assertEqual(r.returncode, 2, r.stderr)

    @unittest.expectedFailure  # verifier finding on 9a756509: git rm into .githooks
    def test_git_rm_of_githooks_is_refused(self):
        r = self.bash("git rm .githooks/pre-commit", cwd=self.wt, proj=self.wt)
        self.assertEqual(r.returncode, 2, r.stderr)


if __name__ == "__main__":
    unittest.main()
