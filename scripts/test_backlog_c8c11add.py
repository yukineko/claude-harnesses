#!/usr/bin/env python3
"""Independent repro for backlog c8c11add (also ac2ba31d).

guard-maintree-edit.py refuses Edit/Write in the MAIN checkout even while a merge
is in progress, so conflict resolution on main is impossible, whereas the commit
guard (check-worktree-isolation.py) allows commits whenever MERGE_HEAD exists.

User ruling 2026-10-04 (the spec pinned here):
  1. MERGE_HEAD present in main's git dir -> edit of ANY path in main's tree ALLOWED.
  2. Only MERGE_HEAD counts: REBASE_HEAD / CHERRY_PICK_HEAD alone stay BLOCKED.
  3. No marker -> BLOCKED (anti-vacuity); linked worktree edits stay allowed.
  4. Undetermined (MERGE_HEAD unreadable / a directory) stays BLOCKED.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
import unittest

SCRIPTS = os.path.dirname(os.path.abspath(__file__))
GUARD = os.path.join(SCRIPTS, "guard-maintree-edit.py")


class MergeHeadEditCarveOut(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.mkdtemp(prefix="c8c11add.")
        self.addCleanup(lambda: subprocess.run(["rm", "-rf", self.tmp]))
        self.addCleanup(lambda: subprocess.run(["chmod", "-R", "u+rwx", self.tmp]))
        # Isolated HOME/TMPDIR so the deny ledger never touches the live one.
        self.home = os.path.join(self.tmp, "home")
        self.tdir = os.path.join(self.tmp, "tmpdir")
        os.makedirs(self.home)
        os.makedirs(self.tdir)
        self.main = os.path.join(self.tmp, "main")
        os.makedirs(self.main)
        self._git(self.main, "init", "-q")
        self._git(self.main, "config", "user.email", "t@t")
        self._git(self.main, "config", "user.name", "t")
        with open(os.path.join(self.main, "tracked.rs"), "w") as f:
            f.write("hi\n")
        self._git(self.main, "add", "-A")
        self._git(self.main, "commit", "-qm", "init")
        self.head = self._git(self.main, "rev-parse", "HEAD")
        self.wt = os.path.join(self.tmp, "wtA")
        self._git(self.main, "worktree", "add", "-q", self.wt, "-b", "feat", "HEAD")
        self.gd = self._git(self.main, "rev-parse", "--absolute-git-dir")
        self.n = 0

    @staticmethod
    def _git(cwd, *args):
        return subprocess.run(
            ["git", *args], cwd=cwd, check=True, capture_output=True, text=True
        ).stdout.strip()

    def _edit_rc(self, path, tool="Edit"):
        self.n += 1
        key = "notebook_path" if tool == "NotebookEdit" else "file_path"
        payload = {
            "tool_name": tool,
            "tool_input": {key: path},
            "session_id": f"sess-{os.path.basename(self.tmp)}-{self.n}",
        }
        env = dict(os.environ)
        env.update(CLAUDE_PROJECT_DIR=self.main, HOME=self.home, TMPDIR=self.tdir)
        p = subprocess.run(
            [sys.executable, GUARD], cwd=self.main, input=json.dumps(payload),
            capture_output=True, text=True, env=env,
        )
        return p.returncode

    def _marker(self, name, content=None):
        with open(os.path.join(self.gd, name), "w") as f:
            f.write((content if content is not None else self.head) + "\n")

    # ---- spec 1: MERGE_HEAD -> allowed (RED before the fix) --------------
    def test_merge_head_allows_edit_of_tracked_file(self):
        self._marker("MERGE_HEAD")
        self.assertEqual(self._edit_rc(os.path.join(self.main, "tracked.rs")), 0)

    def test_merge_head_allows_edit_of_any_path_including_new_file(self):
        self._marker("MERGE_HEAD")
        os.makedirs(os.path.join(self.main, "sub"))
        self.assertEqual(self._edit_rc(os.path.join(self.main, "sub", "new.rs"), "Write"), 0)
        self.assertEqual(self._edit_rc(os.path.join(self.main, "brand_new.rs"), "Write"), 0)

    # ---- spec 2: only MERGE_HEAD counts ----------------------------------
    def test_rebase_head_alone_still_blocks(self):
        self._marker("REBASE_HEAD")
        self.assertEqual(self._edit_rc(os.path.join(self.main, "tracked.rs")), 2)

    def test_cherry_pick_head_alone_still_blocks(self):
        self._marker("CHERRY_PICK_HEAD")
        self.assertEqual(self._edit_rc(os.path.join(self.main, "tracked.rs")), 2)

    # ---- spec 3: controls ------------------------------------------------
    def test_no_marker_blocks(self):
        self.assertEqual(self._edit_rc(os.path.join(self.main, "tracked.rs")), 2)

    def test_worktree_edit_allowed_without_marker(self):
        self.assertEqual(self._edit_rc(os.path.join(self.wt, "tracked.rs")), 0)

    def test_marker_removed_blocks_again(self):
        self._marker("MERGE_HEAD")
        os.remove(os.path.join(self.gd, "MERGE_HEAD"))
        self.assertEqual(self._edit_rc(os.path.join(self.main, "tracked.rs")), 2)

    # ---- spec 4: undetermined stays blocked ------------------------------
    def test_merge_head_is_directory_blocks(self):
        os.makedirs(os.path.join(self.gd, "MERGE_HEAD"))
        self.assertEqual(self._edit_rc(os.path.join(self.main, "tracked.rs")), 2)

    def test_merge_head_unreadable_blocks(self):
        self._marker("MERGE_HEAD")
        os.chmod(os.path.join(self.gd, "MERGE_HEAD"), 0)
        if os.access(os.path.join(self.gd, "MERGE_HEAD"), os.R_OK):
            self.fail("cannot construct an unreadable MERGE_HEAD here (running as root?)")
        self.assertEqual(self._edit_rc(os.path.join(self.main, "tracked.rs")), 2)


if __name__ == "__main__":
    unittest.main()
