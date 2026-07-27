#!/usr/bin/env python3
"""Behavioural tests for the CLAUDE.md §8 worktree-isolation enforcement:

    check-worktree-isolation.py   commit chokepoint (sound, route-independent)
    guard-maintree-edit.py        PreToolUse Edit/Write deny on the main tree
    guard-maintree-bash.py        PreToolUse Bash mutation deny on the main tree

Each test builds a throwaway git repo with a linked worktree and asserts the
exit code, so the RED (blocked) and GREEN (allowed) sides are both pinned. These
are the F→P proofs the scripts were written against.
"""

from __future__ import annotations

import json
import os
import subprocess
import tempfile
import unittest

SCRIPTS = os.path.dirname(os.path.abspath(__file__))


def _run(script: str, cwd: str, payload=None, env_extra=None) -> int:
    env = dict(os.environ)
    if env_extra:
        env.update(env_extra)
    p = subprocess.run(
        ["python3", os.path.join(SCRIPTS, script)],
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

    # ---- edit-time guard (Bash mutations) -------------------------------
    def _bash(self, cmd):
        return {"tool_name": "Bash", "tool_input": {"command": cmd}}

    def test_bash_sed_main_denies(self):
        self.assertEqual(
            _run("guard-maintree-bash.py", self.main,
                 self._bash(f"sed -i s/a/b/ {self.main}/tracked.rs"), self.env), 2)

    def test_bash_rm_main_denies(self):
        self.assertEqual(
            _run("guard-maintree-bash.py", self.main,
                 self._bash(f"rm {self.main}/tracked.rs"), self.env), 2)

    def test_bash_redirect_main_denies(self):
        self.assertEqual(
            _run("guard-maintree-bash.py", self.main,
                 self._bash(f"echo x {chr(62)} {self.main}/new.rs"), self.env), 2)

    def test_bash_git_ops_not_bashguard_denied(self):
        # git subcommands are NOT handled by the Bash guard (they are often
        # recovery / move-to-worktree ops, and any git mutation that reaches main
        # is caught by check-worktree-isolation.py at commit). Over-matching them
        # false-positived on the sanctioned `git stash` + `git worktree` flow.
        for cmd in ("git checkout -- tracked.rs", "git stash push -- tracked.rs",
                    "git worktree add -b b /tmp/x HEAD", "git rm tracked.rs"):
            self.assertEqual(
                _run("guard-maintree-bash.py", self.main,
                     self._bash(cmd), self.env), 0, cmd)

    def test_bash_variable_path_allowed(self):
        # A shell variable cannot be expanded here; refusing it would block
        # legitimate worktree flows. Left to the commit chokepoint.
        self.assertEqual(
            _run("guard-maintree-bash.py", self.main,
                 self._bash("sed -i s/a/b/ $WT/tracked.rs"), self.env), 0)

    def test_bash_move_to_worktree_flow_allowed(self):
        # The exact flow the DENY message recommends must not be self-blocked.
        flow = ('cd %s && git stash push -- f && git worktree add -b b "$WT" HEAD '
                '&& cd "$WT" && git stash apply') % self.main
        self.assertEqual(
            _run("guard-maintree-bash.py", self.main, self._bash(flow), self.env), 0)

    def test_bash_sed_worktree_allows(self):
        self.assertEqual(
            _run("guard-maintree-bash.py", self.main,
                 self._bash(f"sed -i s/a/b/ {self.wt}/tracked.rs"), self.env), 0)

    def test_bash_rm_outside_allows(self):
        self.assertEqual(
            _run("guard-maintree-bash.py", self.main,
                 self._bash(f"rm {self.tmp}/whatever"), self.env), 0)

    def test_bash_read_allows(self):
        self.assertEqual(
            _run("guard-maintree-bash.py", self.main,
                 self._bash(f"cat {self.main}/tracked.rs"), self.env), 0)

    # ---- own-worktree .git/worktrees/<name>/ administrative files --------
    def test_bash_rm_own_worktree_gitdir_lockfile_allowed(self):
        # False positive observed live (backlog): removing a stale index.lock
        # inside THIS worktree's own `.git/worktrees/<name>/` administrative
        # directory (resolved from the worktree's own git-dir) is not a
        # mutation of main's tracked content and must be allowed.
        gd = subprocess.run(
            ["git", "rev-parse", "--absolute-git-dir"],
            cwd=self.wt, capture_output=True, text=True,
        ).stdout.strip()
        lockfile = os.path.join(gd, "index.lock")
        open(lockfile, "w").close()
        self.addCleanup(lambda: os.path.exists(lockfile) and os.remove(lockfile))
        self.assertEqual(
            _run("guard-maintree-bash.py", self.wt,
                 self._bash(f"rm {lockfile}"),
                 {"CLAUDE_PROJECT_DIR": self.main}), 0)

    def test_bash_rm_other_worktree_gitdir_denied(self):
        # A command run from wtA that reaches into a DIFFERENT worktree's
        # `.git/worktrees/<other>/` administrative directory is not "your own
        # worktree" and must still be refused (undetermined -> block, not a
        # blanket allow-all-of-.git/worktrees).
        wt2 = os.path.join(self.tmp, "wtB")
        subprocess.run(
            ["git", "worktree", "add", "-q", wt2, "-b", "feat2", "HEAD"],
            cwd=self.main, check=True,
        )
        gd2 = subprocess.run(
            ["git", "rev-parse", "--absolute-git-dir"],
            cwd=wt2, capture_output=True, text=True,
        ).stdout.strip()
        lockfile2 = os.path.join(gd2, "index.lock")
        open(lockfile2, "w").close()
        self.addCleanup(lambda: os.path.exists(lockfile2) and os.remove(lockfile2))
        self.assertEqual(
            _run("guard-maintree-bash.py", self.wt,
                 self._bash(f"rm {lockfile2}"),
                 {"CLAUDE_PROJECT_DIR": self.main}), 2)

    def test_bash_rm_main_gitconfig_still_denied(self):
        # .git/config of the MAIN tree itself must remain blocked: allowing
        # `.git/` blanket would let a command disable the gate or corrupt the
        # repo. This must not regress when the own-worktree carve-out lands.
        gd = subprocess.run(
            ["git", "rev-parse", "--absolute-git-dir"],
            cwd=self.main, capture_output=True, text=True,
        ).stdout.strip()
        self.assertEqual(
            _run("guard-maintree-bash.py", self.main,
                 self._bash(f"rm {gd}/config"), self.env), 2)

    def test_bash_rm_main_hooks_still_denied(self):
        gd = subprocess.run(
            ["git", "rev-parse", "--absolute-git-dir"],
            cwd=self.main, capture_output=True, text=True,
        ).stdout.strip()
        self.assertEqual(
            _run("guard-maintree-bash.py", self.main,
                 self._bash(f"rm -rf {gd}/hooks"), self.env), 2)


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

    def test_sessionstart_in_worktree_noops(self):
        self.assertEqual(
            _run("session-worktree-init.py", self.wt,
                 {"cwd": self.wt, "session_id": "abcd1234-x"}), 0)

    def test_sessionstart_on_main_creates_worktree(self):
        rc = _run("session-worktree-init.py", self.main,
                  {"cwd": self.main, "session_id": "abcd1234-x"})
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
