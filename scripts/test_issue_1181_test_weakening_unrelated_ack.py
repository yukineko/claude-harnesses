#!/usr/bin/env python3
"""Issue #1181: check-test-weakening reports a file the commit never touched.

scripts/check-test-weakening.py scan() diffs merge-base(origin/main, HEAD)
against the WORKTREE. When the local `main` a branch was forked from is ahead
of a stale `origin/main`, every commit already on main (not on the branch) is
charged to each commit on the branch. In this repo that printed

    [ack] crates/tdd/src/git.rs: ignore-added (...)

on every commit of a worktree that never touched crates/tdd (an `#[ignore]`
that landed on main in db41c92f, acknowledged there).

Fixture: origin/main = C0; local main = C1 adds `#[ignore]` to
crates/tdd/src/git.rs (acknowledged in C1's message); branch `feat` forks from
C1 and changes only docs/unrelated.txt. The scanner is run as the commit-msg
hook runs it (default --base, --pending-msg).

Contract asserted: the scanner's output does not mention
crates/tdd/src/git.rs, a file neither the branch nor this commit changed.

    python3 -m pytest scripts/test_issue_1181_test_weakening_unrelated_ack.py

Written by an independent test author (CLAUDE.md 2.(a)); not by the implementer.
"""
from __future__ import annotations

import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent / "check-test-weakening.py"

RS_BASE = """\
pub fn f() -> u32 { 1 }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one() {
        assert_eq!(f(), 1);
    }
}
"""

RS_IGNORED = RS_BASE.replace("    #[test]\n", "    #[test]\n    #[ignore]\n")


class UnrelatedAckLine(unittest.TestCase):
    def setUp(self) -> None:
        td = tempfile.TemporaryDirectory(prefix="issue1181.")
        self.addCleanup(td.cleanup)
        self.tmp = Path(os.path.realpath(td.name))
        self.home = self.tmp / "home"
        self.home.mkdir()
        self.env = {
            "GIT_AUTHOR_NAME": "t", "GIT_AUTHOR_EMAIL": "t@example.invalid",
            "GIT_COMMITTER_NAME": "t", "GIT_COMMITTER_EMAIL": "t@example.invalid",
            "GIT_CONFIG_GLOBAL": os.devnull, "GIT_CONFIG_NOSYSTEM": "1",
            "HOME": str(self.home), "TMPDIR": str(self.tmp), "LC_ALL": "C",
            "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
        }
        self.repo = self.tmp / "repo"
        self.repo.mkdir()
        self.git("init", "-q", "-b", "main")
        rs = self.repo / "crates" / "tdd" / "src" / "git.rs"
        rs.parent.mkdir(parents=True)
        rs.write_text(RS_BASE)
        (self.repo / "docs").mkdir()
        (self.repo / "docs" / "unrelated.txt").write_text("a\n")
        self.git("add", "-A")
        self.git("commit", "-q", "-m", "C0")
        self.git("update-ref", "refs/remotes/origin/main", "HEAD")
        rs.write_text(RS_IGNORED)
        self.git("add", "-A")
        self.git("commit", "-q", "-m",
                 "C1 ignore\n\ntest-weakening-justified: crates/tdd/src/git.rs:"
                 "ignore-added - parked repro\n")
        self.git("checkout", "-q", "-b", "feat")
        (self.repo / "docs" / "unrelated.txt").write_text("b\n")
        self.git("add", "docs/unrelated.txt")

    def git(self, *args: str) -> str:
        return subprocess.run(["git", *args], cwd=self.repo, env=self.env, check=True,
                              capture_output=True, text=True).stdout

    def test_commit_touching_only_unrelated_file_does_not_report_tdd_git_rs(self) -> None:
        staged = self.git("diff", "--cached", "--name-only").split()
        self.assertEqual(staged, ["docs/unrelated.txt"], "anti-vacuity: commit's own diff")
        branch_diff = self.git("diff", "--name-only", "main", "--").split()
        self.assertEqual(branch_diff, ["docs/unrelated.txt"],
                         "anti-vacuity: the branch never touched crates/tdd")
        msg = self.tmp / "COMMIT_EDITMSG"
        msg.write_text("docs: unrelated\n")
        r = subprocess.run([sys.executable, str(SCRIPT), "--repo", str(self.repo),
                            "--pending-msg", str(msg)],
                           cwd=self.repo, env=self.env, capture_output=True,
                           text=True, timeout=60)
        out = r.stdout + r.stderr
        self.assertIn(r.returncode, (0, 1, 2), out)
        self.assertNotIn("crates/tdd/src/git.rs", out,
                         "a file this commit and branch never touched was reported; "
                         f"rc={r.returncode} output={out!r}")


if __name__ == "__main__":
    unittest.main()
