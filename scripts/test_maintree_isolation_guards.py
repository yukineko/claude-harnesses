#!/usr/bin/env python3
"""Behavioural tests for the CLAUDE.md §8 worktree-isolation enforcement:

    check-worktree-isolation.py   commit chokepoint (sound, route-independent)
    guard-maintree-edit.py        PreToolUse Edit/Write deny on the main tree

(guard-maintree-bash.py no longer predicts from command text; it OBSERVES the
effect of a Bash call. Its tests live in test_guard_maintree_bash_observe.py.)

Each test builds a throwaway git repo with a linked worktree and asserts the
exit code, so the RED (blocked) and GREEN (allowed) sides are both pinned. These
are the F→P proofs the scripts were written against.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
import unittest

SCRIPTS = os.path.dirname(os.path.abspath(__file__))


def _run(script: str, cwd: str, payload=None, env_extra=None) -> int:
    env = dict(os.environ)
    if env_extra:
        env.update(env_extra)
    p = subprocess.run(
        # sys.executable, not "python3": some tests hand the script a PATH that
        # deliberately does not contain git, and resolving the interpreter
        # through that same PATH would break the harness instead of the subject.
        [sys.executable, os.path.join(SCRIPTS, script)],
        cwd=cwd,
        input=(json.dumps(payload) if payload is not None else None),
        capture_output=True,
        text=True,
        env=env,
    )
    return p.returncode


class WorktreeIsolationGuards(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.mkdtemp(prefix="maintree-test.")
        self.addCleanup(lambda: subprocess.run(["rm", "-rf", self.tmp]))
        self.main = os.path.join(self.tmp, "main")
        os.makedirs(self.main)
        subprocess.run(["git", "init", "-q", self.main], check=True)
        for k, v in (("user.email", "t@t"), ("user.name", "t")):
            subprocess.run(["git", "config", k, v], cwd=self.main, check=True)
        open(os.path.join(self.main, "tracked.rs"), "w").write("hi\n")
        subprocess.run(["git", "add", "-A"], cwd=self.main, check=True)
        subprocess.run(["git", "commit", "-qm", "init"], cwd=self.main, check=True)
        self.wt = os.path.join(self.tmp, "wtA")
        subprocess.run(
            ["git", "worktree", "add", "-q", self.wt, "-b", "feat", "HEAD"],
            cwd=self.main, check=True,
        )
        self.env = {"CLAUDE_PROJECT_DIR": self.main}

    # ---- commit chokepoint (sound) --------------------------------------
    def test_commit_main_nonmerge_blocks(self):
        self.assertEqual(_run("check-worktree-isolation.py", self.main), 1)

    def test_commit_worktree_allows(self):
        self.assertEqual(_run("check-worktree-isolation.py", self.wt), 0)

    def test_commit_main_merge_allows(self):
        gd = subprocess.run(
            ["git", "rev-parse", "--absolute-git-dir"],
            cwd=self.main, capture_output=True, text=True,
        ).stdout.strip()
        open(os.path.join(gd, "MERGE_HEAD"), "w").write("x")
        try:
            self.assertEqual(_run("check-worktree-isolation.py", self.main), 0)
        finally:
            os.remove(os.path.join(gd, "MERGE_HEAD"))

    def test_commit_non_repo_blocks_failclosed(self):
        self.assertEqual(_run("check-worktree-isolation.py", self.tmp), 1)

    # ---- edit-time guard (Edit tools) -----------------------------------
    def _edit(self, path):
        return {"tool_name": "Edit", "tool_input": {"file_path": path}}

    def test_edit_main_denies(self):
        self.assertEqual(
            _run("guard-maintree-edit.py", self.main,
                 self._edit(os.path.join(self.main, "tracked.rs")), self.env), 2)

    def test_edit_new_main_file_denies(self):
        self.assertEqual(
            _run("guard-maintree-edit.py", self.main,
                 self._edit(os.path.join(self.main, "brand_new.rs")), self.env), 2)

    def test_edit_worktree_allows(self):
        self.assertEqual(
            _run("guard-maintree-edit.py", self.main,
                 self._edit(os.path.join(self.wt, "tracked.rs")), self.env), 0)

    def test_edit_outside_repo_allows(self):
        self.assertEqual(
            _run("guard-maintree-edit.py", self.main,
                 self._edit(os.path.join(self.tmp, "x.txt")), self.env), 0)

    def test_edit_non_edit_tool_allows(self):
        self.assertEqual(
            _run("guard-maintree-edit.py", self.main,
                 {"tool_name": "Read", "tool_input": {"file_path":
                  os.path.join(self.main, "tracked.rs")}}, self.env), 0)


class LifecycleHooks(WorktreeIsolationGuards):
    """SessionStart auto-worktree and the Stop verify gate."""

    def test_stop_worktree_allows(self):
        self.assertEqual(
            _run("stop-verify-worktree.py", self.wt, {"cwd": self.wt}), 0)

    def test_stop_main_clean_allows(self):
        self.assertEqual(
            _run("stop-verify-worktree.py", self.main, {"cwd": self.main}), 0)

    def test_stop_main_dirty_blocks(self):
        open(os.path.join(self.main, "dirty.rs"), "w").write("x\n")
        self.assertEqual(
            _run("stop-verify-worktree.py", self.main, {"cwd": self.main}), 2)

    def test_stop_dirty_but_active_bounded_allows(self):
        open(os.path.join(self.main, "dirty.rs"), "w").write("x\n")
        self.assertEqual(
            _run("stop-verify-worktree.py", self.main,
                 {"cwd": self.main, "stop_hook_active": True}), 0)

    # ---- stop gate: "git could not answer" is NOT "no repo here" --------
    # CLAUDE.md 3: 判定不能 (IO 失敗 / subprocess の異常終了) は clean ではない.
    # The gate identifies the main tree by comparing --absolute-git-dir with
    # --git-common-dir. If EITHER probe fails for any reason other than "this is
    # not a git repository", the gate cannot tell a worktree from a dirty main
    # tree — and must resolve to the restricted side, not wave the stop through.

    def _shim_git(self, body: str, name: str) -> dict:
        """Return env_extra whose PATH holds ONLY a stub `git` running `body`."""
        bindir = os.path.join(self.tmp, "shimbin-" + name)
        os.makedirs(bindir, exist_ok=True)
        shim = os.path.join(bindir, "git")
        with open(shim, "w") as fh:
            fh.write("#!/bin/sh\n" + body + "\n")
        os.chmod(shim, 0o755)
        return {"PATH": bindir}

    def test_stop_git_erroring_blocks(self):
        # Real, observed shape: a repo git refuses to operate on. Not "no repo".
        env = self._shim_git(
            "echo 'fatal: detected dubious ownership in repository' >&2\nexit 128",
            "dubious",
        )
        self.assertEqual(
            _run("stop-verify-worktree.py", self.main, {"cwd": self.main}, env), 2)

    def test_stop_git_failing_silently_blocks(self):
        env = self._shim_git("exit 1", "silent")
        self.assertEqual(
            _run("stop-verify-worktree.py", self.main, {"cwd": self.main}, env), 2)

    def test_stop_git_unrunnable_blocks(self):
        empty = os.path.join(self.tmp, "emptybin")
        os.makedirs(empty, exist_ok=True)
        self.assertEqual(
            _run("stop-verify-worktree.py", self.main, {"cwd": self.main},
                 {"PATH": empty}), 2)

    def test_stop_outside_any_repo_allows(self):
        # ANTI-VACUITY CONTROL for the three above: the fix must keep ALLOWING
        # the genuinely-not-a-repo case. Resolving every git non-zero to block
        # would trap every stop taken outside a checkout, and would make the
        # three tests above pass for the wrong reason.
        outside = os.path.join(self.tmp, "not-a-repo")
        os.makedirs(outside, exist_ok=True)
        self.assertEqual(
            _run("stop-verify-worktree.py", outside, {"cwd": outside}), 0)

    def _isolated_home(self) -> dict:
        # session-worktree-init.py writes a session registration under
        # $HOME/.backlog/drivers (backlog 491f6e94). Run it against a temp
        # HOME so the suite never writes into the developer's real registry,
        # where a live record reads as an active driver.
        home = os.path.join(self.tmp, "home")
        os.makedirs(home, exist_ok=True)
        return {"HOME": home}

    def test_sessionstart_in_worktree_noops(self):
        self.assertEqual(
            _run("session-worktree-init.py", self.wt,
                 {"cwd": self.wt, "session_id": "abcd1234-x"},
                 self._isolated_home()), 0)

    def test_sessionstart_on_main_creates_worktree(self):
        rc = _run("session-worktree-init.py", self.main,
                  {"cwd": self.main, "session_id": "abcd1234-x"},
                  self._isolated_home())
        self.assertEqual(rc, 0)
        made = os.path.join(
            os.path.dirname(self.main), ".main-worktrees", "session-abcd1234")
        try:
            self.assertTrue(os.path.isdir(made))
        finally:
            subprocess.run(["git", "worktree", "remove", "--force", made],
                           cwd=self.main, capture_output=True)
            subprocess.run(["rm", "-rf", made], capture_output=True)


if __name__ == "__main__":
    unittest.main()
