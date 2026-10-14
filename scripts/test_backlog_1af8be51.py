#!/usr/bin/env python3
"""backlog 1af8be51: scripts/check-worktree-isolation.py decides "a merge is in
progress" with `os.path.exists(<git-dir>/MERGE_HEAD)`. A MERGE_HEAD that is a
DIRECTORY (or a symlink to one) is not git's merge record -- it holds no
commit id -- yet it makes the gate allow an ordinary, non-merge commit in the
main working tree. Its twin, scripts/guard-maintree-edit.py
(`_merge_head_state`, c8c11add), treats exactly these shapes as undetermined
and blocks. CLAUDE.md 3: undetermined resolves to block.

Drives the real script in a throwaway repo's MAIN checkout (no merge, no
pre-merge-commit env), the way .githooks/pre-commit runs it.

Control: a regular MERGE_HEAD file holding a commit id is allowed (exit 0), and
no MERGE_HEAD at all is refused, so the fixture observes both verdicts.

Repro:  python3 -m pytest --runxfail scripts/test_backlog_1af8be51.py
"""

import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent / "check-worktree-isolation.py"


def _env():
    e = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}
    e.pop("CONDUKT_GIT_HOOK", None)  # the pre-merge-commit declaration
    e.update(GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull,
             GIT_AUTHOR_NAME="t", GIT_AUTHOR_EMAIL="t@example.invalid",
             GIT_COMMITTER_NAME="t", GIT_COMMITTER_EMAIL="t@example.invalid")
    return e


class MergeHeadShape(unittest.TestCase):
    def setUp(self):
        self.root = Path(os.path.realpath(tempfile.mkdtemp(prefix="bl-1af8be51-")))
        self.addCleanup(shutil.rmtree, self.root, True)
        self.git("init", "-q", "-b", "main")
        (self.root / "f.txt").write_text("x\n")
        self.git("add", "f.txt")
        self.git("-c", "commit.gpgsign=false", "-c", "core.hooksPath=/dev/null",
                 "commit", "-q", "-m", "seed")
        self.merge_head = self.root / ".git" / "MERGE_HEAD"

    def git(self, *args):
        return subprocess.run(["git", *args], cwd=self.root, env=_env(),
                              capture_output=True, text=True, check=True).stdout

    def gate(self):
        return subprocess.run([sys.executable, str(SCRIPT)], cwd=self.root, env=_env(),
                              capture_output=True, text=True, timeout=60)

    def test_control_no_merge_head_is_refused(self):
        self.assertEqual(self.gate().returncode, 1)

    def test_control_real_merge_head_file_is_allowed(self):
        self.merge_head.write_text(self.git("rev-parse", "HEAD"))
        self.assertEqual(self.gate().returncode, 0)

    @unittest.expectedFailure
    def test_directory_merge_head_does_not_allow_a_main_commit(self):
        self.merge_head.mkdir()
        p = self.gate()
        self.assertNotEqual(
            p.returncode, 0,
            "a DIRECTORY named MERGE_HEAD is no merge record, yet the gate allowed "
            "a non-merge commit in main\n" + p.stderr,
        )

    @unittest.expectedFailure
    def test_symlink_to_directory_merge_head_does_not_allow_a_main_commit(self):
        target = self.root / "somedir"
        target.mkdir()
        self.merge_head.symlink_to(target)
        p = self.gate()
        self.assertNotEqual(p.returncode, 0, p.stderr)


if __name__ == "__main__":
    unittest.main(verbosity=1)
