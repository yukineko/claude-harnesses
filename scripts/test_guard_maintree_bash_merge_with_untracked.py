#!/usr/bin/env python3
"""guard-maintree-bash refuses a genuine two-parent `git merge --no-ff` when main
has a PRE-EXISTING untracked path (observed 2026-10-27, session c5b9c02b).

Observed: right after `git merge --no-ff session-c5b9c02b -m ...` created the
two-parent merge 71706f61 in the main tree, the PostToolUse hook refused with

    Refused: this command modified this project's MAIN working tree outside
    integration (CLAUDE.md 8: main only receives merges).
    Changed in /Users/yuki/src/harness:
      HEAD be502a9120cd -> 71706f61d68a (not a merge / fast-forward)

The message names no path, only HEAD, and main had an untracked `bare.git/` at
its root the whole time. The "HEAD moved => integration" branch of post()
(scripts/guard-maintree-bash.py:393) is guarded by `not after["porcelain"]`,
i.e. it only applies when the WHOLE tree is clean. An untracked path that is
identical at Pre and Post is not a change, yet it keeps `after["porcelain"]`
non-empty, so the merge falls to the final `else: verdict = REFUSE`
(guard-maintree-bash.py:399-400) without integration_head() ever being asked
whether the new HEAD is a merge commit.

Drives the guard exactly like the hook: stdin JSON payload (PreToolUse, then
PostToolUse with the same tool_use_id and the real command text),
CLAUDE_PROJECT_DIR = fixture main repo. The merge is run for real between the
two. Never touches the real main tree (tempfile fixture repo).

    python3 scripts/test_guard_maintree_bash_merge_with_untracked.py

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

CMD = ("git merge --no-ff wtbranch -m 'Merge branch wtbranch\n\n"
       "multi-line message\n\nCo-Authored-By: t <t@example.invalid>'")


class MergeWithPreexistingUntracked(ObserveGuardBase):
    SESSION = "sess-merge-untracked-0001"

    def _side_commit(self) -> None:
        (self.wt / "tracked.txt").write_text("v2 from the worktree\n")
        git(self.wt, "commit", "-q", "-am", "side change")

    def _merge_in_main(self) -> None:
        m = subprocess.run(
            ["git", *GIT_ID, "merge", "-q", "--no-ff", "-m",
             "Merge branch wtbranch\n\nmulti-line message", "wtbranch"],
            cwd=self.main, env=_clean_env(), capture_output=True, text=True)
        self.assertEqual(m.returncode, 0, "anti-vacuity: merge ran: %s" % m.stderr)
        parents = git(self.main, "rev-list", "--parents", "-n1", "HEAD").stdout.split()
        self.assertEqual(len(parents), 3, "anti-vacuity: HEAD is a two-parent merge")

    def _run(self, untracked_setup) -> subprocess.CompletedProcess:
        self._side_commit()
        untracked_setup()
        self.assertTrue(self.status().strip(),
                        "anti-vacuity: main has a pre-existing untracked path")
        tid, _ = self.pre_ok(CMD)
        self._merge_in_main()
        self.assertTrue(self.status().strip(),
                        "anti-vacuity: untracked path still present at Post")
        return self.post(tid, CMD)

    def test_red_merge_allowed_with_untracked_bare_repo_at_root(self) -> None:
        def setup() -> None:
            git(self.tmp, "init", "-q", "--bare", str(self.main / "bare.git"))
        r = self._run(setup)
        self.assertEqual(r.returncode, 0,
                         "genuine two-parent merge refused (rc=%d): %s" % (r.returncode, r.stderr))

    def test_red_merge_allowed_with_plain_untracked_file(self) -> None:
        r = self._run(lambda: (self.main / "scratch.txt").write_text("untracked\n"))
        self.assertEqual(r.returncode, 0,
                         "genuine two-parent merge refused (rc=%d): %s" % (r.returncode, r.stderr))

    def test_control_merge_allowed_with_clean_main(self) -> None:
        self._side_commit()
        tid, _ = self.pre_ok(CMD)
        self._merge_in_main()
        r = self.post(tid, CMD)
        self.assertEqual(r.returncode, 0, r.stderr)


if __name__ == "__main__":
    prog = unittest.main(exit=False, verbosity=2)
    res = prog.result
    failed = len(res.failures) + len(res.errors)
    print("RESULT: %d passed, %d failed" % (res.testsRun - failed - len(res.skipped), failed))
    sys.exit(1 if failed else 0)
