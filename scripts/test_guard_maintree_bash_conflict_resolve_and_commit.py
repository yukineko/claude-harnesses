#!/usr/bin/env python3
"""guard-maintree-bash refuses a call that resolves a conflicted merge AND runs
`git commit` to conclude it (reported 2026-11-07: `git merge --no-ff
condukt-pass-cache` conflicted on versions; the agent resolved it and ran
`git commit -F <file>`; PostToolUse printed "HEAD ef2e90bf0ef0 -> 73a192bebd60
(not classified as merge or fast-forward: the working tree / index also changed
during this call (paths below), so the HEAD move was refused without checking
it)" although 73a192be is a real two-parent merge).

Concluding a conflicted merge is integration, which CLAUDE.md 8 allows on main.

Contract: MERGE_HEAD present at Pre (conflicted, unmerged paths), the observed
call resolves the conflict and commits, resulting HEAD is a two-parent commit
[Pre HEAD, Pre MERGE_HEAD], tree clean at Post -> exit 0 (not refused).

Controls (must stay as stated):
  - plain non-merge commit on main                      -> refused (rc 2)
  - tracked file edited on main, no commit              -> refused (rc 2)
  - conflict-free `git merge --no-ff` in a single call  -> not refused (rc 0)

Only exit codes are asserted, never message text.

    python3 -m pytest scripts/test_guard_maintree_bash_conflict_resolve_and_commit.py -q

Written by a test author who did not implement the guard (CLAUDE.md 2.(a)).
Do NOT weaken the RED test to pass; remove the expectedFailure marker when fixed.
"""
from __future__ import annotations

import subprocess
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from test_guard_maintree_bash_observe import (  # noqa: E402
    GIT_ID,
    ObserveGuardBase,
    _clean_env,
    git,
)

CMD = ("echo resolved > tracked.txt && git add tracked.txt && "
       "git commit -F msg.txt")


class ConflictResolveAndCommit(ObserveGuardBase):
    SESSION = "sess-conflict-resolve-0001"

    def _start_conflicted_merge(self) -> str:
        (self.wt / "tracked.txt").write_text("v2 from the worktree\n")
        (self.wt / "brought_in.txt").write_text("new on the branch\n")
        git(self.wt, "add", "-A")
        git(self.wt, "commit", "-q", "-m", "side change")
        (self.main / "tracked.txt").write_text("v2 conflicting in main\n")
        git(self.main, "commit", "-q", "-am", "main change")
        m = git(self.main, "merge", "--no-ff", "--no-commit", "wtbranch", check=False)
        self.assertNotEqual(m.returncode, 0, "anti-vacuity: merge conflicted")
        self.assertIn("u ", self.status(), "anti-vacuity: unmerged path present")
        mh = self.main / ".git" / "MERGE_HEAD"
        self.assertTrue(mh.exists(), "anti-vacuity: MERGE_HEAD on disk")
        return mh.read_text().split()[0]

    def _real_call_resolve_and_commit(self) -> None:
        (self.main / "tracked.txt").write_text("resolved\n")
        git(self.main, "add", "tracked.txt")
        c = subprocess.run(["git", *GIT_ID, "commit", "-q", "-F", "-"],
                           input="Merge branch wtbranch\n\nresolved\n",
                           cwd=self.main, env=_clean_env(),
                           capture_output=True, text=True)
        self.assertEqual(c.returncode, 0, "anti-vacuity: commit ran: %s" % c.stderr)

    @unittest.expectedFailure
    def test_red_resolve_and_commit_concluding_conflicted_merge_is_allowed(self) -> None:
        merge_head = self._start_conflicted_merge()
        old = git(self.main, "rev-parse", "HEAD").stdout.strip()
        tid, _ = self.pre_ok(CMD)
        self._real_call_resolve_and_commit()
        parents = git(self.main, "rev-list", "--parents", "-n1", "HEAD").stdout.split()
        self.assertEqual(len(parents), 3, "anti-vacuity: HEAD is a two-parent merge")
        self.assertEqual(parents[1], old, "anti-vacuity: first parent is the Pre HEAD")
        self.assertEqual(parents[2], merge_head, "anti-vacuity: second parent is MERGE_HEAD")
        self.assertEqual(self.status(), "", "anti-vacuity: tree clean at Post")
        r = self.post(tid, CMD)
        self.assertEqual(r.returncode, 0,
                         "conflict-resolving merge conclusion refused (rc=%d): %s"
                         % (r.returncode, r.stderr))

    def test_control_plain_noncommit_merge_commit_on_main_is_refused(self) -> None:
        cmd = "echo o2 > other.txt && git add other.txt && git commit -m x"
        self.assertFalse((self.main / ".git" / "MERGE_HEAD").exists())
        old = git(self.main, "rev-parse", "HEAD").stdout.strip()
        tid, _ = self.pre_ok(cmd)
        (self.main / "other.txt").write_text("o2\n")
        git(self.main, "add", "other.txt")
        git(self.main, "commit", "-q", "-m", "x")
        parents = git(self.main, "rev-list", "--parents", "-n1", "HEAD").stdout.split()
        self.assertEqual(parents, [parents[0], old], "anti-vacuity: single-parent commit")
        r = self.post(tid, cmd)
        self.assertEqual(r.returncode, 2, "non-merge commit on main allowed: %s" % r.stderr)

    def test_control_tracked_edit_without_commit_is_refused(self) -> None:
        cmd = "echo edited > tracked.txt"
        tid, _ = self.pre_ok(cmd)
        (self.main / "tracked.txt").write_text("edited\n")
        self.assertIn("tracked.txt", self.status(), "anti-vacuity: tracked file dirty")
        r = self.post(tid, cmd)
        self.assertEqual(r.returncode, 2, "tracked edit on main allowed: %s" % r.stderr)

    def test_control_conflict_free_merge_single_call_is_allowed(self) -> None:
        cmd = "git merge --no-ff wtbranch -m merge"
        (self.wt / "brought_in.txt").write_text("new on the branch\n")
        git(self.wt, "add", "-A")
        git(self.wt, "commit", "-q", "-m", "side change")
        (self.main / "other.txt").write_text("o2 in main\n")
        git(self.main, "commit", "-q", "-am", "main change")
        tid, _ = self.pre_ok(cmd)
        git(self.main, "merge", "--no-ff", "-q", "wtbranch", "-m", "merge")
        parents = git(self.main, "rev-list", "--parents", "-n1", "HEAD").stdout.split()
        self.assertEqual(len(parents), 3, "anti-vacuity: HEAD is a two-parent merge")
        self.assertEqual(self.status(), "", "anti-vacuity: tree clean at Post")
        r = self.post(tid, cmd)
        self.assertEqual(r.returncode, 0, "clean merge refused: %s" % r.stderr)


if __name__ == "__main__":
    prog = unittest.main(exit=False, verbosity=2)
    res = prog.result
    failed = len(res.failures) + len(res.errors)
    print("RESULT: %d passed, %d failed" % (res.testsRun - failed - len(res.skipped), failed))
    sys.exit(1 if failed else 0)
