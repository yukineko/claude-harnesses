#!/usr/bin/env python3
"""Tests for the maintree deny ledger (e033c406), Edit/Write side.

Drives the REAL hook scripts as subprocesses (guard-maintree-edit.py,
stop-verify-worktree.py, deny-ledger-clear.py) in a throwaway repo + linked
worktree, with HOME pointed at a temp dir so the per-session ledger
(`$HOME/.claude/state/maintree-deny/<sid>.jsonl`) never touches the developer's
real one. The interactive/non-interactive split is pinned explicitly through
CLAUDECODE / CLAUDE_CODE_ENTRYPOINT.

User ruling 2026-10-04: the Bash side of e033c406 is dropped. The observing
guard-maintree-bash.py records no refusals, runs no retry gate, and does not
refuse ledger-dir or hook-machinery writes; its tests (and the static guard's
syntax verdicts) were removed. The Edit/Write side is kept, so every deny here
is created through guard-maintree-edit.py and every retry is an Edit/Write
call that the edit guard's own rules allow but that names the denied target.

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

    def edit(self, path: str, sid: str = SID, interactive: bool = False,
             project: str | None = None):
        payload = {"hook_event_name": "PreToolUse", "cwd": self.main,
                   "session_id": sid, "tool_name": "Write",
                   "tool_input": {"file_path": path, "content": "x\n"}}
        return self.run_hook(EDIT, payload, interactive, project)

    def mention(self, text: str, sid: str = SID, interactive: bool = False):
        """An Edit of a WORKTREE file (allowed by the edit guard's own rules)
        whose old_string names `text`."""
        payload = {"hook_event_name": "PreToolUse", "cwd": self.wt,
                   "session_id": sid, "tool_name": "Edit",
                   "tool_input": {"file_path": os.path.join(self.wt, "other.rs"),
                                  "old_string": "see " + text, "new_string": "x"}}
        return self.run_hook(EDIT, payload, interactive)

    def unrelated(self, sid: str = SID, interactive: bool = False):
        """An allowed edit that names no main path."""
        return self.edit(os.path.join(self.wt, "other.rs"), sid=sid,
                         interactive=interactive)

    def stop(self, sid: str = SID, cwd: str | None = None, active: bool = False):
        payload = {"hook_event_name": "Stop", "session_id": sid,
                   "cwd": cwd or self.wt, "stop_hook_active": active}
        return self.run_hook(STOP, payload, cwd=cwd or self.wt)

    def clear(self, sid: str = SID):
        payload = {"hook_event_name": "UserPromptSubmit", "session_id": sid,
                   "cwd": self.main, "prompt": "next instruction"}
        return self.run_hook(CLEAR, payload)

    def deny_main(self, sid: str = SID):
        r = self.edit(self.target, sid=sid)
        self.assertEqual(r.returncode, 2, "control: Write into main is denied")
        return r

    def entries(self) -> list[dict]:
        with open(self.ledger) as f:
            return [json.loads(l) for l in f if l.strip()]


class DenyIsRecorded(_Fixture):
    def test_edit_deny_writes_a_ledger_entry_under_home(self):
        self.deny_main()
        denies = [e for e in self.entries() if e["kind"] == "deny"]
        self.assertEqual(len(denies), 1)
        e = denies[0]
        self.assertEqual(e["target_abs"], self.target)
        self.assertEqual(e["root"], self.main)
        self.assertEqual(e["denier"], "guard-maintree-edit.py")
        self.assertIn("Write " + self.target, e["raw"])
        self.assertTrue(e["reason"])
        self.assertTrue(e["snapshot"]["exists"])
        self.assertEqual(e["snapshot"]["status"], "")
        # Never in the project root (CLAUDE.md §5).
        self.assertFalse(os.path.exists(os.path.join(self.main, ".claude")))

    def test_allowed_calls_with_no_ledger_write_nothing(self):
        r = self.unrelated()
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertFalse(os.path.exists(self.ledger))


class RetryIsRefused(_Fixture):
    def test_retry_naming_the_denied_target_is_refused_quoting_the_deny(self):
        self.deny_main()
        # A worktree edit is allowed by the guard's own rules; only the ledger
        # refuses it, because its text names the denied main path.
        retry = self.mention(self.target)
        self.assertEqual(retry.returncode, 2, retry.stderr)
        self.assertIn("guard-maintree-edit.py refused `Write " + self.target + "`",
                      retry.stderr)
        self.assertIn("This call reaches the same target " + self.target +
                      " by another spelling; confirm it is genuinely a "
                      "different approach.", retry.stderr)

    def test_retry_is_an_ask_in_an_interactive_session(self):
        self.deny_main()
        retry = self.mention(self.target, interactive=True)
        self.assertEqual(retry.returncode, 0, retry.stderr)
        out = json.loads(retry.stdout)["hookSpecificOutput"]
        self.assertEqual(out["permissionDecision"], "ask")
        self.assertEqual(out["hookEventName"], "PreToolUse")
        self.assertIn("refused `Write " + self.target + "`",
                      out["permissionDecisionReason"])

    def test_retry_through_write_content_is_refused(self):
        self.deny_main()
        r = self.run_hook(EDIT, {
            "session_id": SID, "tool_name": "Write", "cwd": self.wt,
            "tool_input": {"file_path": os.path.join(self.wt, "n.md"),
                           "content": "see " + self.target}})
        self.assertEqual(r.returncode, 2, r.stderr)
        self.assertIn("by another spelling", r.stderr)

    def test_worktree_redo_is_allowed(self):
        self.deny_main()
        redo = self.edit(os.path.join(self.wt, "tracked.rs"))
        self.assertEqual(redo.returncode, 0, redo.stderr)
        self.assertEqual(redo.stdout, "")

    def test_a_longer_name_sharing_the_prefix_is_not_the_target(self):
        self.deny_main()
        r = self.mention(self.target + ".orig " + self.target + "2")
        self.assertEqual(r.returncode, 0, r.stderr)

    def test_a_different_session_is_unaffected(self):
        self.deny_main()
        r = self.mention(self.target, sid="another-session")
        self.assertEqual(r.returncode, 0, r.stderr)

    def test_the_window_expires_by_time(self):
        self.deny_main()
        lines = self.entries()
        for e in lines:
            e["epoch"] -= 3600
        with open(self.ledger, "w") as f:
            f.write("".join(json.dumps(e) + "\n" for e in lines))
        r = self.mention(self.target)
        self.assertEqual(r.returncode, 0, r.stderr)

    def test_the_window_expires_by_call_count(self):
        self.deny_main()
        with open(self.ledger, "a") as f:
            for _ in range(25):
                f.write(json.dumps({"v": 1, "kind": "tick", "epoch": time.time()}) + "\n")
        r = self.mention(self.target)
        self.assertEqual(r.returncode, 0, r.stderr)

    def test_the_window_expires_after_25_real_guarded_calls(self):
        self.deny_main()
        for _ in range(25):
            self.assertEqual(self.unrelated().returncode, 0)
        self.assertEqual(self.mention(self.target).returncode, 0)

    def test_calls_inside_the_window_are_counted(self):
        self.deny_main()
        self.assertEqual(self.unrelated().returncode, 0)
        self.assertEqual([e["kind"] for e in self.entries()], ["deny", "tick"])


class UndeterminedLedgerRefuses(_Fixture):
    def _corrupt(self, text: str) -> None:
        os.makedirs(os.path.dirname(self.ledger), exist_ok=True)
        with open(self.ledger, "w") as f:
            f.write(text)

    def test_corrupt_ledger_refuses_an_unrelated_call(self):
        self._corrupt("{this is not json\n")
        r = self.unrelated()
        self.assertEqual(r.returncode, 2, r.stderr)
        self.assertIn("could not be consulted", r.stderr)

    def test_corrupt_ledger_asks_in_an_interactive_session(self):
        self._corrupt('{"v": 1, "kind": "deny"}\n')  # missing fields
        r = self.unrelated(interactive=True)
        self.assertEqual(r.returncode, 0)
        self.assertEqual(json.loads(r.stdout)["hookSpecificOutput"]
                         ["permissionDecision"], "ask")

    def test_unreadable_ledger_refuses(self):
        os.makedirs(self.ledger)  # a directory where the file should be
        r = self.unrelated()
        self.assertEqual(r.returncode, 2, r.stderr)

    def test_unusable_session_id_refuses(self):
        r = self.unrelated(sid="../escape")
        self.assertEqual(r.returncode, 2, r.stderr)

    def test_missing_ledger_is_no_prior_denies(self):
        r = self.unrelated()
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.stop().returncode, 0)

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
        self.deny_main()
        self.assertTrue(os.path.exists(self.ledger))
        self.assertEqual(self.mention(self.target).returncode, 2,
                         "control: the retry is refused before the clear")
        r = self.clear()
        self.assertEqual(r.returncode, 0)
        self.assertEqual(r.stdout, "")
        self.assertFalse(os.path.exists(self.ledger))
        self.assertEqual(self.mention(self.target).returncode, 0)

    def test_clear_leaves_other_sessions_alone(self):
        self.deny_main(sid="other-session")
        self.clear()
        other = os.path.join(os.path.dirname(self.ledger), "other-session.jsonl")
        self.assertTrue(os.path.exists(other))


class StopDetectsChange(_Fixture):
    def test_stop_blocks_when_a_denied_path_changed(self):
        self.deny_main()
        os.remove(self.target)  # the refused effect, by a route no guard saw
        r = self.stop()
        self.assertEqual(r.returncode, 2, r.stderr)
        self.assertIn("guard-maintree-edit.py refused `Write " + self.target + "`",
                      r.stderr)

    def test_stop_blocks_when_a_denied_path_was_rewritten(self):
        self.deny_main()
        with open(self.target, "w") as f:
            f.write("changed\n")
        self.assertEqual(self.stop().returncode, 2)

    def test_stop_blocks_when_the_change_came_through_a_git_route(self):
        self.deny_main()
        self.assertEqual(self.stop().returncode, 0)
        with open(os.path.join(self.wt, "tracked.rs"), "w") as f:
            f.write("changed\n")
        self.git("commit", "-qam", "c", cwd=self.wt)
        self.git("checkout", "feat", "--", "tracked.rs")
        r = self.stop()
        self.assertEqual(r.returncode, 2, r.stderr)
        self.assertIn("REFUSED", r.stderr)

    def test_stop_allows_when_the_denied_path_is_unchanged(self):
        self.deny_main()
        r = self.stop()
        self.assertEqual(r.returncode, 0, r.stderr)

    def test_stop_allows_a_change_that_landed_as_a_commit(self):
        # A merge on main moves HEAD and leaves the path clean: not this gate's
        # call (check-worktree-isolation.py judges how HEAD moved).
        self.deny_main()
        with open(self.target, "w") as f:
            f.write("merged\n")
        self.git("commit", "-qam", "land")
        self.assertEqual(self.stop().returncode, 0)

    def test_stop_hook_active_is_the_bounded_allow(self):
        self.deny_main()
        os.remove(self.target)
        self.assertEqual(self.stop(active=True).returncode, 0)

    def test_other_session_stop_is_unaffected(self):
        self.deny_main()
        os.remove(self.target)
        self.assertEqual(self.stop(sid="another-session").returncode, 0)


class HookMachineryIsProtected(_Fixture):
    def test_edit_tool_into_hook_machinery_is_refused_in_any_tree(self):
        # `.git/hooks` / `.git/config` stay refused from any tree and anchor.
        for project in (self.wt, self.main):
            for path in (os.path.join(self.main, ".git", "hooks", "pre-commit"),
                         os.path.join(self.main, ".git", "config")):
                with self.subTest(path=path, project=project):
                    r = self.edit(path, project=project)
                    self.assertEqual(r.returncode, 2, r.stderr)
                    self.assertIn("hook machinery", r.stderr)

    def test_edit_tool_into_worktree_githooks_is_allowed(self):
        # User rulings 2026-10-03/04: refusing an edit to the tracked
        # `.githooks` inside a linked worktree is itself the defect.
        for project in (self.wt, self.main):
            with self.subTest(project=project):
                r = self.edit(os.path.join(self.wt, ".githooks", "pre-commit"),
                              sid="wt-githooks-" + os.path.basename(project),
                              project=project)
                self.assertEqual(r.returncode, 0, r.stderr)

    def test_edit_tool_into_main_githooks_is_still_refused(self):
        # Control: main's `.githooks` is refused by the general main-tree rule.
        r = self.edit(os.path.join(self.main, ".githooks", "pre-commit"),
                      project=self.main)
        self.assertEqual(r.returncode, 2, r.stderr)
        self.assertIn("MAIN working tree", r.stderr)
        # A worktree path that symlinks into main's `.githooks` is judged by
        # its realpath and refused the same way.
        link = os.path.join(self.wt, "hooks-link")
        os.symlink(os.path.join(self.main, ".githooks"), link)
        r = self.edit(os.path.join(link, "pre-commit"), sid="symlink-sid",
                      project=self.main)
        self.assertEqual(r.returncode, 2, r.stderr)
        self.assertIn("MAIN working tree", r.stderr)


class LedgerDirSelfProtection(_Fixture):
    def test_edit_into_the_ledger_dir_is_refused_from_any_tree(self):
        r = self.edit(os.path.join(self.home, ".claude", "state", "maintree-deny",
                                   "x.jsonl"), sid="Le", project=self.wt)
        self.assertEqual(r.returncode, 2, r.stderr)
        self.assertIn("deny ledger directory", r.stderr)
        r = self.edit(os.path.join(self.wt, "tracked.rs"), sid="Le2", project=self.wt)
        self.assertEqual(r.returncode, 0, r.stderr)


class LedgerFallbackAndBounds(_Fixture):
    def _fallback(self) -> str:
        return os.path.join(self.tmpdir, "maintree-deny-%d" % os.getuid(),
                            SID + ".jsonl")

    def _fallback_dir(self) -> str:
        return os.path.dirname(self._fallback())

    def _unwritable_home(self) -> None:
        # HOME is a regular FILE: `<HOME>/.claude/...` can never be created.
        shutil.rmtree(self.home)
        with open(self.home, "w") as f:
            f.write("not a directory\n")

    def test_unwritable_home_records_to_the_fallback_and_still_refuses(self):
        self._unwritable_home()
        self.deny_main()
        self.assertTrue(os.path.exists(self._fallback()))
        retry = self.mention(self.target)
        self.assertEqual(retry.returncode, 2, retry.stderr)
        self.assertIn("by another spelling", retry.stderr)
        os.remove(self.target)
        self.assertEqual(self.stop().returncode, 2)
        self.clear()
        self.assertFalse(os.path.exists(self._fallback()))

    def test_both_ledgers_are_read(self):
        # A deny that landed in the fallback is seen even once HOME works.
        self._unwritable_home()
        self.deny_main()
        os.remove(self.home)
        os.makedirs(self.home)
        self.assertEqual(self.mention(self.target).returncode, 2)

    def test_no_writable_location_keeps_the_deny_and_says_so(self):
        self._unwritable_home()
        # The fallback DIRECTORY is a regular file, so it cannot be created.
        # (A read-only TMPDIR would not do: tempfile.gettempdir() then falls
        # back to /tmp on its own.)
        with open(self._fallback_dir(), "w") as f:
            f.write("not a directory\n")
        r = self.edit(self.target)
        self.assertEqual(r.returncode, 2)
        self.assertIn("could NOT be written to the deny ledger", r.stderr)

    def test_fallback_dir_not_0700_is_not_used(self):
        self._unwritable_home()
        os.mkdir(self._fallback_dir(), 0o755)
        os.chmod(self._fallback_dir(), 0o755)
        r = self.edit(self.target)
        self.assertEqual(r.returncode, 2)
        self.assertIn("could NOT be written to the deny ledger", r.stderr)
        self.assertEqual(os.listdir(self._fallback_dir()), [])

    def test_symlinked_fallback_dir_is_not_used(self):
        self._unwritable_home()
        elsewhere = os.path.join(self.tmp, "elsewhere")
        os.mkdir(elsewhere, 0o700)
        os.symlink(elsewhere, self._fallback_dir())
        r = self.edit(self.target)
        self.assertEqual(r.returncode, 2)
        self.assertIn("could NOT be written to the deny ledger", r.stderr)
        self.assertEqual(os.listdir(elsewhere), [])

    def test_symlinked_ledger_file_is_not_followed(self):
        os.makedirs(os.path.dirname(self.ledger))
        decoy = os.path.join(self.tmp, "decoy.jsonl")
        open(decoy, "w").close()
        os.symlink(decoy, self.ledger)
        r = self.unrelated()
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

    def test_clear_does_not_reach_through_a_symlinked_fallback_dir(self):
        elsewhere = os.path.join(self.tmp, "elsewhere")
        os.mkdir(elsewhere, 0o700)
        victim = os.path.join(elsewhere, SID + ".jsonl")
        open(victim, "w").close()
        os.symlink(elsewhere, self._fallback_dir())
        self.assertEqual(self.clear().returncode, 0)
        self.assertTrue(os.path.exists(victim))

    def _corrupt(self, age_secs: float) -> None:
        os.makedirs(os.path.dirname(self.ledger), exist_ok=True)
        with open(self.ledger, "w") as f:
            f.write("{garbage\n")
        t = time.time() - age_secs
        os.utime(self.ledger, (t, t))

    def test_corrupt_ledger_refuses_inside_the_bound(self):
        self._corrupt(19 * 60)
        r = self.unrelated()
        self.assertEqual(r.returncode, 2, r.stderr)
        self.assertIn("refused until", r.stderr)
        self.assertEqual(self.stop().returncode, 2)

    def test_corrupt_ledger_is_moved_aside_after_the_bound(self):
        self._corrupt(21 * 60)
        r = self.unrelated()
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

    def test_stale_corrupt_ledger_salvages_its_valid_denies(self):
        self.deny_main()
        with open(self.ledger, "a") as f:
            f.write("{garbage\n")
        with open(self.ledger) as f:
            n_lines = sum(1 for _ in f)
        old = time.time() - 21 * 60
        os.utime(self.ledger, (old, old))
        r = self.unrelated()
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("moved it aside", r.stderr)
        self.assertIn(f"dropped unparseable line(s): {n_lines}", r.stderr)
        denies = [e for e in self.entries() if e["kind"] == "deny"]
        self.assertEqual([e["target_abs"] for e in denies], [self.target])
        # The salvaged deny still refuses a retry and still feeds Stop.
        self.assertEqual(self.mention(self.target).returncode, 2)
        with open(self.target, "a") as f:
            f.write("changed\n")
        self.assertEqual(self.stop().returncode, 2)

    def test_raw_and_reason_are_truncated(self):
        long_target = os.path.join(self.main, "y" * 200 + ".rs")
        self.assertEqual(self.edit(long_target).returncode, 2)
        e = [x for x in self.entries() if x["kind"] == "deny"][0]
        self.assertLessEqual(len(e["raw"]), 120)
        self.assertLessEqual(len(e["reason"]), 300)
        self.assertNotIn("\n", e["reason"])

    # RESIDUAL kept on purpose: a vanished root blocks Stop until clear.
    def test_deny_whose_root_vanished_blocks_stop_until_clear(self):
        self.deny_main()
        shutil.rmtree(self.main)
        r = self.stop(cwd=self.tmp)
        self.assertEqual(r.returncode, 2, r.stderr)
        self.assertIn("cannot be read now", r.stderr)
        r = self.run_hook(CLEAR, {"hook_event_name": "UserPromptSubmit",
                                  "session_id": SID, "cwd": self.tmp,
                                  "prompt": "next"}, cwd=self.tmp)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.stop(cwd=self.tmp).returncode, 0)


if __name__ == "__main__":
    unittest.main()
