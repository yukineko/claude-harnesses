#!/usr/bin/env python3
"""Adversarial tests for guard-maintree-edit.py after 6316fec1 (written by an
independent verifier, not the implementer).

6316fec1 dropped `.githooks` from the any-tree hook-machinery rule so that the
tracked `.githooks` inside a linked worktree is editable, and claims main's
`.githooks` stays refused "by the main-tree rule" while `.git/hooks`,
`.git/config` and `config.worktree` stay refused in any tree.

The implementer's tests cover only the Write tool and only main's `.githooks`
under a MAIN project anchor. Here:

  * Edit / MultiEdit / Write (and a new file) into <wt>/.githooks: allowed
    under both anchors (RED on the base, which refused `.githooks` anywhere);
  * main's `.githooks` refused under BOTH anchors, including a session whose
    CLAUDE_PROJECT_DIR is the worktree, directly and through a worktree
    symlink (the any-tree rule used to catch this regardless of anchor);
  * `.git/hooks` and `config.worktree` in the worktree's own gitdir
    (<main>/.git/worktrees/<wt>/) and `<wt>/.git/hooks/...` refused under both
    anchors.

    python3 -m unittest scripts.test_guard_maintree_edit_githooks_adv
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from test_deny_ledger_e033c406 import EDIT, _Fixture  # noqa: E402


class WorktreeGithooksAdversarial(_Fixture):
    def call(self, tool: str, path: str, project: str, sid: str = "adv-sid"):
        if tool == "Write":
            ti = {"file_path": path, "content": "x\n"}
        elif tool == "Edit":
            ti = {"file_path": path, "old_string": "exit 0", "new_string": "exit 1"}
        else:  # MultiEdit
            ti = {"file_path": path,
                  "edits": [{"old_string": "exit 0", "new_string": "exit 1"}]}
        payload = {"hook_event_name": "PreToolUse", "cwd": project,
                   "session_id": sid, "tool_name": tool, "tool_input": ti}
        return subprocess.run(
            [sys.executable, EDIT], cwd=project, input=json.dumps(payload),
            capture_output=True, text=True,
            env=self.env(False, project), timeout=60)

    def anchors(self):
        return (("wt", self.wt), ("main", self.main))

    def test_edit_multiedit_write_into_worktree_githooks_allowed(self):
        existing = os.path.join(self.wt, ".githooks", "pre-commit")
        new_file = os.path.join(self.wt, ".githooks", "pre-push")
        for name, project in self.anchors():
            for tool in ("Write", "Edit", "MultiEdit"):
                for path in (existing, new_file):
                    with self.subTest(tool=tool, anchor=name, path=path):
                        r = self.call(tool, path, project,
                                      sid=f"allow-{tool}-{name}")
                        self.assertEqual(r.returncode, 0, r.stderr)

    def test_main_githooks_refused_under_both_anchors(self):
        target = os.path.join(self.main, ".githooks", "pre-commit")
        for name, project in self.anchors():
            for tool in ("Write", "Edit", "MultiEdit"):
                with self.subTest(tool=tool, anchor=name):
                    r = self.call(tool, target, project,
                                  sid=f"main-{tool}-{name}")
                    self.assertEqual(r.returncode, 2,
                                     f"main .githooks edit with anchor={name} "
                                     f"was not refused; stderr={r.stderr!r}")

    def test_worktree_symlink_into_main_githooks_refused_under_both_anchors(self):
        link = os.path.join(self.wt, "hooks-link")
        os.symlink(os.path.join(self.main, ".githooks"), link)
        target = os.path.join(link, "pre-commit")
        for name, project in self.anchors():
            with self.subTest(anchor=name):
                r = self.call("Edit", target, project, sid=f"link-{name}")
                self.assertEqual(r.returncode, 2,
                                 f"symlink into main .githooks with anchor={name} "
                                 f"was not refused; stderr={r.stderr!r}")

    def test_git_hooks_and_worktree_gitdir_config_refused(self):
        gitdir = subprocess.run(
            ["git", "rev-parse", "--absolute-git-dir"], cwd=self.wt,
            capture_output=True, text=True, check=True).stdout.strip()
        self.assertIn(os.sep + "worktrees" + os.sep, gitdir)
        paths = (
            os.path.join(self.wt, ".git", "hooks", "pre-commit"),
            os.path.join(self.main, ".git", "hooks", "pre-push"),
            os.path.join(self.main, ".git", "config"),
            os.path.join(gitdir, "config.worktree"),
            os.path.join(self.main, ".git", "config.worktree"),
        )
        for name, project in self.anchors():
            for path in paths:
                with self.subTest(anchor=name, path=path):
                    r = self.call("Write", path, project, sid=f"gm-{name}")
                    self.assertEqual(r.returncode, 2, r.stderr)
                    self.assertIn("hook machinery", r.stderr)


if __name__ == "__main__":
    unittest.main()
