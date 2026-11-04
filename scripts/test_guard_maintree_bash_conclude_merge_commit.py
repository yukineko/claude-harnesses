#!/usr/bin/env python3
"""guard-maintree-bash refuses `git commit` that CONCLUDES a merge started in an
EARLIER Bash call (observed 2026-11-04 in the main tree).

Observed sequence in main:
  call A: `git merge --no-ff --no-commit <branch>` -> conflicts
  call B..: conflicts resolved with `git checkout <ref> -- <paths>`, all staged
            (MERGE_HEAD on disk throughout)
  call N: ONLY `git commit -F - <<EOF ... EOF` -> 2-parent merge commit, clean tree
The PostToolUse phase of call N refused it verbatim:

    Refused: this command modified this project's MAIN working tree outside
    integration ... HEAD c34956ea9ca3 -> 2957604f4b6a (not classified as merge
    or fast-forward: the working tree / index also changed during this call
    (paths below), so the HEAD move was refused without checking it)

and listed every path the merge brought in. At Pre the merged paths are staged
(dirty vs HEAD); at Post they are committed (clean), so the dirty sets differ,
only_unchanged_preexisting() is False and post() falls to the final REFUSE
without asking whether the new HEAD is the merge MERGE_HEAD pointed at.

Contract: MERGE_HEAD present at Pre, resulting HEAD a merge commit whose second
parent is that MERGE_HEAD, tree clean at Post -> ALLOW.
Controls (must stay REFUSED): no merge in progress, a tracked file is modified
and staged, then `git commit` makes an ordinary single-parent commit.

Drives the guard like the hook (stdin JSON, Pre then Post with the same
tool_use_id, CLAUDE_PROJECT_DIR = tempfile fixture repo). Never touches the
real main tree.

    python3 -m pytest scripts/test_guard_maintree_bash_conclude_merge_commit.py -q

Written by a test author who did not implement the guard (CLAUDE.md 2.(a)).
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

COMMIT_CMD = ("git commit -F - <<'EOF'\nMerge branch wtbranch\n\n"
              "conflicts resolved in an earlier call\nEOF")


class ConcludeMergeWithCommit(ObserveGuardBase):
    SESSION = "sess-conclude-merge-0001"

    def _commit_via_stdin(self, message: str) -> None:
        c = subprocess.run(["git", *GIT_ID, "commit", "-q", "-F", "-"],
                           input=message, cwd=self.main, env=_clean_env(),
                           capture_output=True, text=True)
        self.assertEqual(c.returncode, 0, "anti-vacuity: commit ran: %s" % c.stderr)

    def _start_conflicted_merge_and_resolve(self) -> str:
        """Earlier calls (outside this Pre/Post): conflicted merge, resolved by
        `git checkout wtbranch -- <paths>`, everything staged. Returns the
        MERGE_HEAD oid."""
        (self.wt / "tracked.txt").write_text("v2 from the worktree\n")
        (self.wt / "brought_in.txt").write_text("new on the branch\n")
        git(self.wt, "add", "-A")
        git(self.wt, "commit", "-q", "-m", "side change")
        (self.main / "tracked.txt").write_text("v2 conflicting in main\n")
        git(self.main, "commit", "-q", "-am", "main change")
        m = git(self.main, "merge", "--no-ff", "--no-commit", "wtbranch", check=False)
        self.assertNotEqual(m.returncode, 0, "anti-vacuity: merge conflicted")
        self.assertIn("u ", self.status(), "anti-vacuity: unmerged path present")
        git(self.main, "checkout", "wtbranch", "--", "tracked.txt")
        st = self.status()
        self.assertNotIn("\nu ", "\n" + st, "anti-vacuity: conflicts resolved")
        self.assertIn("tracked.txt", st, "anti-vacuity: resolution staged")
        self.assertIn("brought_in.txt", st, "anti-vacuity: merged path staged")
        mh = self.main / ".git" / "MERGE_HEAD"
        self.assertTrue(mh.exists(), "anti-vacuity: MERGE_HEAD on disk before the call")
        oid = mh.read_text().split()[0]
        self.assertEqual(oid, git(self.main, "rev-parse", "wtbranch").stdout.strip())
        return oid

    def test_red_commit_concluding_in_progress_merge_is_allowed(self) -> None:
        merge_head = self._start_conflicted_merge_and_resolve()
        old = git(self.main, "rev-parse", "HEAD").stdout.strip()
        tid, _ = self.pre_ok(COMMIT_CMD)
        self._commit_via_stdin("Merge branch wtbranch\n\nconflicts resolved earlier\n")
        parents = git(self.main, "rev-list", "--parents", "-n1", "HEAD").stdout.split()
        self.assertEqual(len(parents), 3, "anti-vacuity: HEAD is a two-parent merge")
        self.assertEqual(parents[1], old, "anti-vacuity: first parent is the Pre HEAD")
        self.assertEqual(parents[2], merge_head,
                         "anti-vacuity: second parent is the Pre MERGE_HEAD")
        self.assertFalse((self.main / ".git" / "MERGE_HEAD").exists(),
                         "anti-vacuity: merge concluded")
        self.assertEqual(self.status(), "", "anti-vacuity: tree clean at Post")
        r = self.post(tid, COMMIT_CMD)
        self.assertEqual(r.returncode, 0,
                         "commit concluding an in-progress merge refused (rc=%d): %s"
                         % (r.returncode, r.stderr))

    def test_control_ordinary_commit_staged_in_call_is_refused(self) -> None:
        cmd = "echo o2 > other.txt && git add other.txt && git commit -m x"
        self.assertFalse((self.main / ".git" / "MERGE_HEAD").exists())
        old = git(self.main, "rev-parse", "HEAD").stdout.strip()
        tid, _ = self.pre_ok(cmd)
        (self.main / "other.txt").write_text("o2\n")
        git(self.main, "add", "other.txt")
        self._commit_via_stdin("ordinary commit\n")
        parents = git(self.main, "rev-list", "--parents", "-n1", "HEAD").stdout.split()
        self.assertEqual(parents, [parents[0], old], "anti-vacuity: single-parent commit")
        self.assertEqual(self.status(), "", "anti-vacuity: tree clean at Post")
        r = self.post(tid, cmd)
        self.assertEqual(r.returncode, 2, "ordinary commit in main allowed: %s" % r.stderr)
        self.assertIn("Refused", r.stderr)

    def test_control_ordinary_commit_of_prestaged_change_is_refused(self) -> None:
        # Closest non-merge twin of the contract shape: dirty+staged at Pre,
        # `git commit` alone in the call, HEAD moved, clean at Post, but no
        # MERGE_HEAD and a single-parent result.
        (self.main / "other.txt").write_text("o2\n")
        git(self.main, "add", "other.txt")
        self.assertIn("other.txt", self.status(), "anti-vacuity: staged at Pre")
        self.assertFalse((self.main / ".git" / "MERGE_HEAD").exists())
        old = git(self.main, "rev-parse", "HEAD").stdout.strip()
        tid, _ = self.pre_ok(COMMIT_CMD)
        self._commit_via_stdin("ordinary commit\n")
        parents = git(self.main, "rev-list", "--parents", "-n1", "HEAD").stdout.split()
        self.assertEqual(parents, [parents[0], old], "anti-vacuity: single-parent commit")
        self.assertEqual(self.status(), "", "anti-vacuity: tree clean at Post")
        r = self.post(tid, COMMIT_CMD)
        self.assertEqual(r.returncode, 2, "ordinary commit in main allowed: %s" % r.stderr)
        self.assertIn("Refused", r.stderr)


if __name__ == "__main__":
    prog = unittest.main(exit=False, verbosity=2)
    res = prog.result
    failed = len(res.failures) + len(res.errors)
    print("RESULT: %d passed, %d failed" % (res.testsRun - failed - len(res.skipped), failed))
    sys.exit(1 if failed else 0)
