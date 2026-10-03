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
        self.tmpdir = os.path.join(self.tmp, "tmpdir")
        os.makedirs(self.tmpdir)
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
        # The fallback ledger lives under tempfile.gettempdir(): keep it in
        # this fixture too.
        env["TMPDIR"] = self.tmpdir
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



class VerifierFindingsClosed(_Fixture):
    """Defects the independent verifier found on 9a756509, plus the
    coordinator's follow-ups (variable-held keys, parsed GIT_CONFIG_* words,
    git subcommands writing hook dirs, hooks-only substitutions and globs)."""

    def _refused(self, cmds, cwd=None, project=None):
        for cmd in cmds:
            with self.subTest(cmd=cmd, project=project, cwd=cwd):
                r = self.bash(cmd, cwd=cwd or self.wt, project=project)
                self.assertEqual(r.returncode, 2, r.stderr)

    def _allowed(self, cmds, cwd=None, project=None):
        for cmd in cmds:
            with self.subTest(cmd=cmd, project=project, cwd=cwd):
                r = self.bash(cmd, cwd=cwd or self.wt, project=project)
                self.assertEqual(r.returncode, 0, r.stderr)

    def test_variable_and_unknown_config_keys_are_refused(self):
        for project in (None, self.wt):
            self._refused([
                'k=core.hooksPath; git config "$k" /dev/null',
                'k=core.hooksPath; git config set "$k" /dev/null',
                'git config "$UNKNOWN_KEY" /dev/null',
                'k=core.hooksPath; git -c "$k=/dev/null" status',
                'git -c "$UNKNOWN=/dev/null" status',
                'git -c "core.hooks""Path=/dev/null" status',
                'git --config-env=core.hooksPath=EVIL status',
                'git --config-env "$K=EVIL" status',
            ], project=project)
        self._allowed(['k=core.hooksPath; git config --get "$k"',
                       'k=user.name; git config "$k" me'])

    def test_git_config_env_words_are_judged_parsed(self):
        self._refused([
            'GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=core.hooks""Path '
            'GIT_CONFIG_VALUE_0=/dev/null git commit -m a',
            'export GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=core.hooks""Path '
            'GIT_CONFIG_VALUE_0=/dev/null; git commit -m a',
            'GIT_CONFIG_KEY_0=core.hooks""Path; export GIT_CONFIG_KEY_0',
            'env GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=core.hooks""Path '
            'GIT_CONFIG_VALUE_0=/dev/null git commit -m a',
            'GIT_CONFIG_KEY_0="$K" git commit -m a',
            "GIT_CONFIG_PARAMETERS=\"'core.hooks''Path'='/dev/null'\" git commit -m a",
        ])
        self._allowed(['GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=user.name '
                       'GIT_CONFIG_VALUE_0=me git commit -m a',
                       'GIT_CONFIG_GLOBAL=/dev/null git status'])

    def test_git_subcommands_writing_hook_dirs_are_refused(self):
        for project, cwd in ((None, self.wt), (self.wt, self.wt), (None, self.main)):
            self._refused([
                "git rm .githooks/pre-commit",
                "git rm -r .githooks",
                "git checkout -- .githooks/pre-commit",
                "git checkout HEAD -- .githooks",
                "git restore .githooks/pre-commit",
                "git restore --source HEAD~1 -- .githooks/pre-commit",
                "git mv .githooks/pre-commit x",
                "git clean -fdx .githooks",
                "git stash push -- .githooks",
                "git stash -- .githooks/pre-commit",
                "git apply --directory=.githooks p.diff",
                "git am --directory .git/hooks p.mbox",
                "git checkout -- '.githook*'",
                "git checkout -- ':(top).githooks/pre-commit'",
                "cd .githooks && git rm pre-commit",
                "git -C .githooks rm pre-commit",
            ], cwd=cwd, project=project)

    def test_broad_git_forms_stay_allowed(self):
        for cwd in (self.wt, self.main):
            self._allowed([
                "git checkout .",
                "git reset --hard",
                "git reset --hard HEAD~0",
                "git stash",
                "git stash pop",
                "git merge feat",
                "git apply p.diff",
                "git checkout feat -- tracked.rs",
                "git restore tracked.rs",
                "git stash push -m 'fix .githooks wording' -- tracked.rs",
                "git checkout -- ':!.githooks'",
            ], cwd=cwd)

    def test_hooks_only_mode_sees_git_dir_substitutions_and_globs(self):
        self._refused([
            'echo x > "$(git rev-parse --git-common-dir)/hooks/pre-commit"',
            "cp /dev/null $(git rev-parse --git-dir)/config",
            "echo x >> `git rev-parse --absolute-git-dir`/config",
            'rm "$(git rev-parse --git-path hooks)/pre-commit"',
            "rm .githook*/pre-commit",
            "rm -rf .git/hoo*",
            "chmod -x .githook?/*",
        ], project=self.wt)
        self._allowed([
            "cat $(git rev-parse --git-dir)/config",
            "rm -rf ./*",
            "echo x > $(git rev-parse --show-toplevel)/notes.txt",
        ], project=self.wt)


class LedgerFallbackAndBounds(_Fixture):
    def _fallback(self) -> str:
        return os.path.join(self.tmpdir, "maintree-deny-%d" % os.getuid(),
                            SID + ".jsonl")

    def _unwritable_home(self) -> None:
        # HOME is a regular FILE: `<HOME>/.claude/...` can never be created.
        shutil.rmtree(self.home)
        with open(self.home, "w") as f:
            f.write("not a directory\n")

    def test_unwritable_home_records_to_the_fallback_and_still_refuses(self):
        self._unwritable_home()
        self.deny_rm()
        self.assertTrue(os.path.exists(self._fallback()))
        retry = self.bash("cat " + self.target)
        self.assertEqual(retry.returncode, 2, retry.stderr)
        self.assertIn("by another spelling", retry.stderr)
        os.remove(self.target)
        self.assertEqual(self.stop().returncode, 2)
        self.clear()
        self.assertFalse(os.path.exists(self._fallback()))

    def test_both_ledgers_are_read(self):
        # A deny that landed in the fallback is seen even once HOME works.
        self._unwritable_home()
        self.deny_rm()
        os.remove(self.home)
        os.makedirs(self.home)
        self.assertEqual(self.bash("cat " + self.target).returncode, 2)

    def test_no_writable_location_keeps_the_deny_and_says_so(self):
        self._unwritable_home()
        # The fallback DIRECTORY is a regular file, so it cannot be created.
        # (A read-only TMPDIR would not do: tempfile.gettempdir() then falls
        # back to /tmp on its own.)
        with open(os.path.dirname(self._fallback()), "w") as f:
            f.write("not a directory\n")
        r = self.bash("rm " + self.target)
        self.assertEqual(r.returncode, 2)
        self.assertIn("could NOT be written to the deny ledger", r.stderr)

    def _corrupt(self, age_secs: float) -> None:
        os.makedirs(os.path.dirname(self.ledger), exist_ok=True)
        with open(self.ledger, "w") as f:
            f.write("{garbage\n")
        t = time.time() - age_secs
        os.utime(self.ledger, (t, t))

    def test_corrupt_ledger_refuses_inside_the_bound(self):
        self._corrupt(19 * 60)
        r = self.bash("ls /")
        self.assertEqual(r.returncode, 2, r.stderr)
        self.assertIn("refused until", r.stderr)
        self.assertEqual(self.stop().returncode, 2)

    def test_corrupt_ledger_is_moved_aside_after_the_bound(self):
        self._corrupt(21 * 60)
        r = self.bash("ls /")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("moved it aside", r.stderr)
        self.assertFalse(os.path.exists(self.ledger))
        aside = [n for n in os.listdir(os.path.dirname(self.ledger))
                 if n.startswith(SID + ".jsonl.corrupt-")]
        self.assertEqual(len(aside), 1)
        self.assertIn(aside[0], r.stderr)
        self.assertEqual(self.stop().returncode, 0)

    def test_stale_corrupt_ledger_is_moved_aside_at_stop_too(self):
        self._corrupt(21 * 60)
        r = self.stop()
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("moved it aside", r.stderr)

    def test_raw_and_reason_are_truncated(self):
        long_cmd = "rm " + self.target + " " + " ".join(["x"] * 200)
        self.assertEqual(self.bash(long_cmd).returncode, 2)
        e = [x for x in self.entries() if x["kind"] == "deny"][0]
        self.assertLessEqual(len(e["raw"]), 120)
        self.assertLessEqual(len(e["reason"]), 300)
        self.assertNotIn("\n", e["reason"])


class Round3Findings(_Fixture):
    """Verifier round 3 (verify2 / 0de9f3c0) and the coordinator's items 1-6."""

    def _refused_in_wt(self, cmd: str, sid: str = "r3") -> None:
        r = self.bash(cmd, sid=sid, cwd=self.wt, project=self.wt)
        self.assertEqual(r.returncode, 2, f"{cmd!r} must be refused: {r.stderr}")

    def _allowed_in_wt(self, cmd: str, sid: str = "r3ok") -> None:
        r = self.bash(cmd, sid=sid, cwd=self.wt, project=self.wt)
        self.assertEqual(r.returncode, 0, f"{cmd!r} must be allowed: {r.stderr}")

    # item 1
    def test_glob_pathspec_matching_a_real_hook_file_is_refused(self):
        for i, cmd in enumerate(("git checkout HEAD -- '*pre-commit'",
                                 "git rm '*pre-commit'",
                                 "git checkout HEAD -- ':(icase)*PRE-COMMIT'",
                                 "git restore -s HEAD ':(top)*commit'")):
            self._refused_in_wt(cmd, sid=f"g{i}")
        for cmd in ("git rm '*.orig'", "git checkout HEAD -- '*.rs'",
                    "git checkout HEAD -- ':(literal)*pre-commit'"):
            self._allowed_in_wt(cmd)

    def test_glob_pathspec_with_unlistable_hook_dir_is_refused(self):
        hooks = os.path.join(self.wt, ".githooks")
        os.chmod(hooks, 0)
        self.addCleanup(os.chmod, hooks, 0o755)
        self._refused_in_wt("git rm '*.orig'")
        self._allowed_in_wt("git rm tracked.rs")  # not a glob: nothing to list

    # item 2
    def test_patch_mode_dash_p_takes_no_value(self):
        for i, cmd in enumerate(("git restore -s HEAD -p .githooks",
                                 "git stash push -p .githooks",
                                 "git checkout -p .githooks")):
            self._refused_in_wt(cmd, sid=f"p{i}")
        self._allowed_in_wt("git apply -p1 x.patch")
        self._allowed_in_wt("git apply -p 1 x.patch")

    # item 3
    def test_stale_corrupt_ledger_salvages_its_valid_denies(self):
        self.deny_rm()
        with open(self.ledger, "a") as f:
            f.write("{garbage\n")
        with open(self.ledger) as f:
            n_lines = sum(1 for _ in f)
        old = time.time() - 21 * 60
        os.utime(self.ledger, (old, old))
        r = self.bash("ls /")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("moved it aside", r.stderr)
        self.assertIn(f"dropped unparseable line(s): {n_lines}", r.stderr)
        denies = [e for e in self.entries() if e["kind"] == "deny"]
        self.assertEqual([e["target_abs"] for e in denies], [self.target])
        # The salvaged deny still refuses a retry and still feeds Stop.
        self.assertEqual(self.bash("cat " + self.target).returncode, 2)
        with open(self.target, "a") as f:
            f.write("changed\n")
        self.assertEqual(self.stop().returncode, 2)

    # item 4
    def test_writes_into_the_ledger_dirs_are_refused_from_any_tree(self):
        led = "~/.claude/state/maintree-deny"
        fb = "$TMPDIR/maintree-deny-%d" % os.getuid()
        for i, cmd in enumerate((f"rm -rf {led}", f"rm {led}/x.jsonl",
                                 f"touch {led}/x.jsonl", f"chmod 600 {led}/x.jsonl",
                                 f"mv {led} {self.wt}/y", f"echo x > {led}/x.jsonl",
                                 "rm -rf ~/.claude", "rm -rf ~/.claude/stat*",
                                 f"rm -rf {fb}", "rm -rf $TMPDIR/*",
                                 f"echo x > {fb}/x.jsonl")):
            self._refused_in_wt(cmd, sid=f"L{i}")
        os.makedirs(os.path.join(self.home, ".claude", "state", "maintree-deny"),
                    exist_ok=True)
        for cmd in (f"cat {led}/x.jsonl", f"ls {led}", "rm -rf ~/.claude/other",
                    "touch $TMPDIR/scratch.txt"):
            self._allowed_in_wt(cmd)
        r = self.edit(os.path.join(self.home, ".claude", "state", "maintree-deny",
                                   "x.jsonl"), sid="Le", project=self.wt)
        self.assertEqual(r.returncode, 2, r.stderr)
        self.assertIn("deny ledger directory", r.stderr)
        r = self.edit(os.path.join(self.wt, "tracked.rs"), sid="Le2", project=self.wt)
        self.assertEqual(r.returncode, 0, r.stderr)

    # item 5
    def _fallback_dir(self) -> str:
        return os.path.join(self.tmpdir, "maintree-deny-%d" % os.getuid())

    def _home_unwritable(self) -> None:
        shutil.rmtree(self.home)
        with open(self.home, "w") as f:
            f.write("not a directory\n")

    def test_fallback_dir_not_0700_is_not_used(self):
        self._home_unwritable()
        os.mkdir(self._fallback_dir(), 0o755)
        os.chmod(self._fallback_dir(), 0o755)
        r = self.bash("rm " + self.target)
        self.assertEqual(r.returncode, 2)
        self.assertIn("could NOT be written to the deny ledger", r.stderr)
        self.assertEqual(os.listdir(self._fallback_dir()), [])

    def test_symlinked_fallback_dir_is_not_used(self):
        self._home_unwritable()
        elsewhere = os.path.join(self.tmp, "elsewhere")
        os.mkdir(elsewhere, 0o700)
        os.symlink(elsewhere, self._fallback_dir())
        r = self.bash("rm " + self.target)
        self.assertEqual(r.returncode, 2)
        self.assertIn("could NOT be written to the deny ledger", r.stderr)
        self.assertEqual(os.listdir(elsewhere), [])

    def test_symlinked_ledger_file_is_not_followed(self):
        os.makedirs(os.path.dirname(self.ledger))
        decoy = os.path.join(self.tmp, "decoy.jsonl")
        open(decoy, "w").close()
        os.symlink(decoy, self.ledger)
        r = self.bash("ls /")
        self.assertEqual(r.returncode, 2, "an unreadable ledger refuses")
        self.assertEqual(os.path.getsize(decoy), 0)

    def test_tempdir_never_falls_back_to_the_cwd(self):
        sys.path.insert(0, SCRIPTS)
        self.addCleanup(sys.path.remove, SCRIPTS)
        import deny_ledger
        from unittest import mock
        with mock.patch.object(deny_ledger.os.path, "isdir", return_value=False):
            self.assertIsNone(deny_ledger._tempdir())
            self.assertIsNone(deny_ledger.fallback_dir())
            self.assertIsNone(deny_ledger.fallback_path("s"))

    # item 6
    def test_git_dir_substitution_anywhere_in_the_text(self):
        for i, cmd in enumerate((
                'echo x > "$(cd . && git rev-parse --git-dir)/config"',
                "echo x > $(cd . && git rev-parse --git-common-dir)/hooks/pre-commit",
                'touch "$(true; git rev-parse --git-path hooks)/post-commit"')):
            self._refused_in_wt(cmd, sid=f"s{i}")

    # GIT_CONFIG_KEY_n filled by printf -v / read
    def test_printf_v_and_read_into_git_config_key_are_judged(self):
        for i, cmd in enumerate((
                "printf -v GIT_CONFIG_KEY_0 %s core.hooksPath; git status",
                "read GIT_CONFIG_KEY_0 <<< core.hooksPath; git status",
                "read GIT_CONFIG_PARAMETERS; git status")):
            self._refused_in_wt(cmd, sid=f"k{i}")
        self._allowed_in_wt("printf -v FOO %s bar; read BAR <<< baz; git status")

    # RESIDUAL kept on purpose: a vanished root blocks Stop until clear.
    def test_deny_whose_root_vanished_blocks_stop_until_clear(self):
        self.deny_rm()
        shutil.rmtree(self.main)
        r = self.stop(cwd=self.tmp)
        self.assertEqual(r.returncode, 2, r.stderr)
        self.assertIn("cannot be read now", r.stderr)
        r = self.run_hook(CLEAR, {"hook_event_name": "UserPromptSubmit",
                                  "session_id": SID, "cwd": self.tmp,
                                  "prompt": "next"}, cwd=self.tmp)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.stop(cwd=self.tmp).returncode, 0)

    def test_clear_does_not_reach_through_a_symlinked_fallback_dir(self):
        elsewhere = os.path.join(self.tmp, "elsewhere")
        os.mkdir(elsewhere, 0o700)
        victim = os.path.join(elsewhere, SID + ".jsonl")
        open(victim, "w").close()
        os.symlink(elsewhere, self._fallback_dir())
        self.assertEqual(self.clear().returncode, 0)
        self.assertTrue(os.path.exists(victim))

    def test_value_options_before_patch_mode_from_both_cwds(self):
        # The verifier's exact command, plus the glued / `=` value spellings,
        # with cwd and project anchor at main and at the worktree.
        cmds = ("git restore -s HEAD~1 -p .githooks",
                "git restore -sHEAD~1 -p .githooks",
                "git restore --source=HEAD~1 -p .githooks",
                "git restore --source HEAD~1 -p .githooks",
                "git checkout -b nb -p .githooks",
                "git checkout --orphan nb -- .githooks",
                "git checkout --conflict merge -p .githooks",
                "git stash push -m wip -p .githooks",
                "git stash push --message=wip -p .githooks",
                "git clean -e x -f .githooks")
        for where in (self.main, self.wt):
            for i, cmd in enumerate(cmds):
                with self.subTest(cwd=where, cmd=cmd):
                    r = self.bash(cmd, sid=f"v{i}x{len(where)}", cwd=where,
                                  project=where)
                    self.assertEqual(r.returncode, 2, r.stderr)
        for cmd in ("git restore -s HEAD~1 -p src", "git rm --cached -r -q build",
                    "git mv -k a.rs b.rs", "git clean -e x.keep -fd out"):
            self._allowed_in_wt(cmd)

    def test_dot_relative_pathspecs_are_normalised(self):
        os.makedirs(os.path.join(self.wt, "src"), exist_ok=True)
        for i, cmd in enumerate((
                "git checkout HEAD -- './*pre-commit'",
                "cd src && git checkout HEAD -- '../*pre-commit'",
                "cd src && git rm '.././.githooks/*'",
                "git checkout HEAD -- ./.githooks/pre-commit",
                "cd src && git checkout HEAD -- ../.githooks",
                "cd src && git checkout HEAD -- '../../*'")):
            self._refused_in_wt(cmd, sid=f"n{i}")
        for cmd in ("git checkout HEAD -- './*.rs'",
                    "cd src && git checkout HEAD -- '../*.rs'"):
            self._allowed_in_wt(cmd)


if __name__ == "__main__":
    unittest.main()
