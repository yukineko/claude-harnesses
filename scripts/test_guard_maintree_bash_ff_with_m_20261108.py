#!/usr/bin/env python3
"""RED repro: guard-maintree-bash refuses a genuine fast-forward made by
`git merge -m <msg> <tip>`.

Observed 2026-11-08: PostToolUse refused with
  "HEAD 656327ce1e0c -> cb6755f4b9e0 (not a merge / fast-forward)"
while main's reflog entry was verbatim
  "merge cb6755f4...: Fast-forward (no commit created; -m option ignored)"
(git appends that suffix when `-m` is given and the merge fast-forwards;
`condukt` worktree merge does exactly that). integration_step() requires
message.endswith(": Fast-forward"), so the suffix fails it.

Control: a direct commit on main is still refused.

Repro:  python3 -m pytest --runxfail scripts/test_guard_maintree_bash_ff_with_m_20261108.py -k ff_with_m
Written by a test author who did not implement the guard (CLAUDE.md 2.(a)).
"""
from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from test_guard_maintree_bash_observe import ObserveGuardBase, git  # noqa: E402


class FfWithM(ObserveGuardBase):
    def _side_commit(self) -> None:
        (self.wt / "tracked.txt").write_text("v2 from side\n")
        git(self.wt, "add", "tracked.txt")
        git(self.wt, "commit", "-q", "-m", "side work", "--", "tracked.txt")

    def test_ff_with_m_is_integration(self) -> None:
        self._side_commit()
        old = git(self.main, "rev-parse", "HEAD").stdout.strip()
        cmd = 'git merge --no-edit -m "Merge branch x" wtbranch'
        tid, _ = self.pre_ok(cmd)
        git(self.main, "merge", "-q", "--no-edit", "-m", "Merge branch x", "wtbranch")
        new = git(self.main, "rev-parse", "HEAD").stdout.strip()
        self.assertNotEqual(new, old, "anti-vacuity: HEAD moved")
        self.assertEqual(len(git(self.main, "rev-list", "--parents", "-n1", "HEAD")
                             .stdout.split()), 2, "anti-vacuity: fast-forward, no merge commit")
        common = git(self.main, "rev-parse", "--git-common-dir").stdout.strip()
        log = (self.main / common / "logs" / "refs" / "heads" / "main").read_text()
        self.assertIn("Fast-forward (no commit created; -m option ignored)", log,
                      "anti-vacuity: reflog carries the -m suffix")
        self.assertEqual(self.status(), "", "anti-vacuity: tree clean")
        r = self.post(tid, cmd)
        self.assertEqual(r.returncode, 0,
                         "genuine fast-forward (merge -m) refused: %s" % r.stderr)

    def test_control_direct_commit_refused(self) -> None:
        cmd = "git commit -m direct -- tracked.txt"
        tid, _ = self.pre_ok(cmd)
        (self.main / "tracked.txt").write_text("direct\n")
        git(self.main, "add", "tracked.txt")
        git(self.main, "commit", "-q", "-m", "direct commit on main", "--", "tracked.txt")
        self.assert_blocked_d(self.post(tid, cmd))


if __name__ == "__main__":
    import unittest
    unittest.main()
