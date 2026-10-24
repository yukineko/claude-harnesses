#!/usr/bin/env python3
"""Issue #1186: revisions created or moved earlier in the same Bash command.

The static guard's KNOWN HOLES (4b0e5ab3) listed two routes it could not see:

  * `git fetch ... && git checkout FETCH_HEAD -- <pathspec>`
  * `git checkout x && git checkout - -- <pathspec>`

The guard was replaced by the effect observer (b8390f7a). These tests drive
both routes through the REAL scripts/guard-maintree-bash.py (Pre, then the
command, then Post) and assert the Post refuses (exit 2) and names the main
file the command modified. Neither route is in the corpus of
test_guard_maintree_bash_observe.py.

    python3 -m pytest scripts/test_issue_1186_guard_bash_rev_routes.py

Written by an independent test author (CLAUDE.md 2.(a)); not by the implementer.
"""
from __future__ import annotations

import subprocess
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from test_guard_maintree_bash_observe import ObserveGuardBase, _clean_env, git  # noqa: E402


class RevisionRoutesAreObserved(ObserveGuardBase):
    def _sh(self, cmd: str) -> None:
        env = _clean_env()
        env["HOME"] = str(self.tmp)
        subprocess.run(["bash", "-c", cmd], cwd=self.main, env=env, check=True,
                       capture_output=True, text=True)

    def _post(self, tid: str, cmd: str):
        return self.post(tid, cmd, env_override={"HOME": str(self.tmp)})

    def test_fetch_head_checkout_into_main_is_refused(self) -> None:
        (self.wt / "tracked.txt").write_text("from-wt\n")
        git(self.wt, "commit", "-q", "-am", "wt change")
        cmd = "git fetch -q . wtbranch && git checkout FETCH_HEAD -- '*.txt'"
        tid, _ = self.pre_ok(cmd)
        self._sh(cmd)
        self.assertEqual((self.main / "tracked.txt").read_text(), "from-wt\n",
                         "anti-vacuity: the command modified a main file")
        self.assert_blocked_d(self._post(tid, cmd), "tracked.txt")

    def test_checkout_dash_into_main_is_refused(self) -> None:
        git(self.main, "branch", "-q", "x")
        git(self.main, "checkout", "-q", "x")
        (self.main / "tracked.txt").write_text("on-x\n")
        git(self.main, "commit", "-q", "-am", "x change")
        git(self.main, "checkout", "-q", "main")
        self.assertEqual(self.status(), "", "anti-vacuity: main clean before the call")
        # The command itself moves to x and back, so its `-` names x, a
        # revision the same command set.
        cmd = "git checkout -q x && git checkout -q main && git checkout - -- '*.txt'"
        tid, _ = self.pre_ok(cmd)
        self._sh(cmd)
        self.assertEqual((self.main / "tracked.txt").read_text(), "on-x\n",
                         "anti-vacuity: the command modified a main file")
        self.assertEqual(git(self.main, "symbolic-ref", "HEAD").stdout.strip(),
                         "refs/heads/main")
        self.assert_blocked_d(self._post(tid, cmd), "tracked.txt")


if __name__ == "__main__":
    unittest.main()
