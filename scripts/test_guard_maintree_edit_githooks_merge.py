#!/usr/bin/env python3
"""Tests for the merge exemption of guard-maintree-edit.py's
`_primary_githooks_root` rule (added in 8b400415; written by an independent
test author, not the implementer).

The rule: an Edit/Write/MultiEdit into the `.githooks` of a PRIMARY checkout
(the holder of `.githooks` has a `.git` DIRECTORY) is refused regardless of the
CLAUDE_PROJECT_DIR anchor, EXCEPT while a merge is in progress in that checkout:
a readable MERGE_HEAD lifts the refusal, an unreadable one refuses as
undetermined. User ruling 2026-10-04: keep the exemption, test it.

Every case runs under BOTH anchors (CLAUDE_PROJECT_DIR = main, = worktree):

  1. main mid-merge (a real `git merge --no-commit`, and separately a written
     MERGE_HEAD holding a valid sha) -> Write/Edit/MultiEdit into
     <main>/.githooks/x allowed (exit 0);
  2. no MERGE_HEAD -> refused (exit 2);
  3. MERGE_HEAD present but unreadable (chmod 000) -> refused (exit 2);
  4. MERGE_HEAD in the WORKTREE's gitdir only -> main's `.githooks` still
     refused;
  5. control: <wt>/.githooks allowed with and without any MERGE_HEAD.

The guard under test defaults to the real scripts/guard-maintree-edit.py; set
GUARD_MAINTREE_EDIT_UNDER_TEST to a path to run these tests against a mutant
(this is how the RED run was observed).

    python3 -m unittest scripts.test_guard_maintree_edit_githooks_merge
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from test_deny_ledger_e033c406 import EDIT, _Fixture  # noqa: E402

GUARD = os.environ.get("GUARD_MAINTREE_EDIT_UNDER_TEST") or EDIT
TOOLS = ("Write", "Edit", "MultiEdit")


class PrimaryGithooksMergeExemption(_Fixture):
    def setUp(self) -> None:
        super().setUp()
        self.main_gitdir = os.path.join(self.main, ".git")
        self.wt_gitdir = subprocess.run(
            ["git", "rev-parse", "--absolute-git-dir"], cwd=self.wt,
            capture_output=True, text=True, check=True).stdout.strip()
        self.assertNotEqual(os.path.realpath(self.wt_gitdir),
                            os.path.realpath(self.main_gitdir))
        # Main's `.githooks/x`: an existing tracked file and a new one.
        self.main_targets = (
            os.path.join(self.main, ".githooks", "pre-commit"),
            os.path.join(self.main, ".githooks", "x"),
        )

    # -- helpers ---------------------------------------------------------
    def call(self, tool: str, path: str, project: str, sid: str):
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
            [sys.executable, GUARD], cwd=project, input=json.dumps(payload),
            capture_output=True, text=True,
            env=self.env(False, project), timeout=60)

    def anchors(self):
        return (("main", self.main), ("wt", self.wt))

    def head_sha(self, cwd: str) -> str:
        return subprocess.run(["git", "rev-parse", "HEAD"], cwd=cwd,
                              capture_output=True, text=True,
                              check=True).stdout.strip()

    def start_real_merge_in_main(self) -> None:
        """A real `git merge --no-commit --no-ff` in main, leaving MERGE_HEAD."""
        with open(os.path.join(self.wt, "feat.rs"), "w") as f:
            f.write("feat\n")
        self.git("add", "feat.rs", cwd=self.wt)
        self.git("commit", "-qm", "feat", cwd=self.wt)
        self.git("merge", "--no-commit", "--no-ff", "feat")
        self.assertTrue(os.path.isfile(os.path.join(self.main_gitdir, "MERGE_HEAD")),
                        "precondition: git merge left MERGE_HEAD in main")

    def write_merge_head(self, gitdir: str) -> str:
        marker = os.path.join(gitdir, "MERGE_HEAD")
        with open(marker, "w") as f:
            f.write(self.head_sha(self.main) + "\n")
        return marker

    def assert_all(self, targets, expected: int, label: str) -> None:
        for name, project in self.anchors():
            for tool in TOOLS:
                for path in targets:
                    with self.subTest(case=label, anchor=name, tool=tool, path=path):
                        r = self.call(tool, path, project,
                                      sid=f"{label}-{name}-{tool}")
                        self.assertEqual(
                            r.returncode, expected,
                            f"{label}: {tool} {path} with anchor={name} exited "
                            f"{r.returncode}, expected {expected}; "
                            f"stderr={r.stderr!r}")

    # -- 1. merge in progress lifts the refusal --------------------------
    def test_real_merge_in_main_allows_main_githooks(self):
        self.start_real_merge_in_main()
        self.assert_all(self.main_targets, 0, "real-merge")

    def test_written_merge_head_in_main_allows_main_githooks(self):
        self.write_merge_head(self.main_gitdir)
        self.assert_all(self.main_targets, 0, "written-merge-head")

    # -- 2. no merge -> refused ------------------------------------------
    def test_no_merge_head_refuses_main_githooks(self):
        self.assertFalse(os.path.lexists(os.path.join(self.main_gitdir, "MERGE_HEAD")))
        self.assert_all(self.main_targets, 2, "no-merge-head")

    def test_refusal_returns_after_merge_concludes(self):
        self.start_real_merge_in_main()
        self.git("merge", "--abort")
        self.assertFalse(os.path.lexists(os.path.join(self.main_gitdir, "MERGE_HEAD")))
        self.assert_all(self.main_targets, 2, "merge-aborted")

    # -- 3. unreadable MERGE_HEAD -> refused -----------------------------
    def test_unreadable_merge_head_refuses_main_githooks(self):
        marker = self.write_merge_head(self.main_gitdir)
        os.chmod(marker, 0)
        self.addCleanup(os.chmod, marker, 0o644)
        # Precondition, not a skip: if this process can still read it (e.g.
        # running as root) the case is not exercised and must fail loudly.
        with self.assertRaises(OSError, msg="precondition: MERGE_HEAD unreadable"):
            with open(marker, "rb") as f:
                f.read(1)
        self.assert_all(self.main_targets, 2, "unreadable-merge-head")

    # -- 4. a worktree merge does not lift main's refusal ----------------
    def test_merge_head_in_worktree_does_not_lift_main_githooks(self):
        self.write_merge_head(self.wt_gitdir)
        self.assertFalse(os.path.lexists(os.path.join(self.main_gitdir, "MERGE_HEAD")))
        self.assert_all(self.main_targets, 2, "wt-merge-head")

    # -- 5. control: worktree .githooks allowed regardless ---------------
    def test_worktree_githooks_allowed_regardless_of_merge_head(self):
        wt_targets = (os.path.join(self.wt, ".githooks", "pre-commit"),
                      os.path.join(self.wt, ".githooks", "x"))
        self.assert_all(wt_targets, 0, "wt-control-none")
        self.write_merge_head(self.wt_gitdir)
        self.assert_all(wt_targets, 0, "wt-control-wt-merge")
        os.remove(os.path.join(self.wt_gitdir, "MERGE_HEAD"))
        self.write_merge_head(self.main_gitdir)
        self.assert_all(wt_targets, 0, "wt-control-main-merge")


if __name__ == "__main__":
    unittest.main()
