#!/usr/bin/env python3
"""RED repro: check-test-weakening judges the WORKING TREE, not the INDEX,
so it blocks commits on unstaged changes and passes staged weakenings that
the working tree hides.

Observed 2026-11-08: a commit whose index held only a format-only change to
another test file plus version files was blocked with

    check-test-weakening: undetermined — crates/blastguard/tests/heredoc_body_gt_fp_20261107.rs: #[test] inside a macro_rules! template in a file that exists at the base -- the change is not one of the proven-safe shapes (invocation counting included), so removed generated tests cannot be ruled out

heredoc_body_gt_fp_20261107.rs was only modified-unstaged.

Code read at main 18e4c6f7:
  * the gate runs from .githooks/commit-msg (`python3 "$SCANNER" --repo
    "$REPO" --pending-msg "$MSG_FILE"`), not from pre-commit;
  * scripts/weakening_vcs.py `changed_pairs` runs `git diff --name-status -z -M
    <merge_base>` with no `--cached`, which compares merge-base with the WORKING
    TREE;
  * the "after" side of every comparison is `worktree_content` /
    `worktree_or_none`, which read the file from disk.
No documented reason was found for judging the working tree rather than the
index. The docstring's "vs the worktree" belongs to the branch/worktree SCOPE
ruling (#1181, which rejected `origin/main` as the base); it says nothing about
staged vs unstaged content.

Contract: the gate judges what the commit will contain, which is the index.
  RED      * an unstaged weakening in test file A, with only unrelated file B
             staged, is not reported (rc 0, A absent from the output). Shown
             with an `#[ignore]` added to A, and with A being a macro_rules!
             template file (the observed Undetermined).
  RED      * a weakening staged in A while the working-tree copy is restored
             to the original still blocks. Today this is a FAIL-OPEN: the gate
             reads the clean disk copy and passes. The coordinator expected
             this to pass today; it does not.
  RED      * partial staging, where the index differs from both HEAD and the
             working tree:
             (a) index = weakened, worktree = weakening undone plus an
                 unrelated new test. Must block; today it passes (fail-open).
             (b) index = a harmless added test, worktree = that plus an
                 unstaged `#[ignore]`. Must be clean; today it blocks.
  CONTROL  * the same weakening, staged AND in the working tree, blocks
             (ignore-added). Passes today.
  CONTROL  * B staged alone, A untouched, is clean. Passes today.

Note for the fix: `git commit -a` and `git commit <paths>` run commit-msg
with GIT_INDEX_FILE pointing at a temporary index, so "the index" means the
one git hands the hook. This fixture stages with `git add` and the default
index only.

Set CTW_SCRIPT to run against another copy of the gate.

Repro:  python3 -m pytest --runxfail scripts/test_check_test_weakening_index_not_worktree.py
"""
from __future__ import annotations

import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(os.environ.get(
    "CTW_SCRIPT", str(Path(__file__).resolve().parent / "check-test-weakening.py")))

A = "crates/c/tests/a.rs"
B = "crates/c/notes.txt"

A_BASE = """\
#[test]
fn one() {
    assert_eq!(1, 1);
}

#[test]
fn two() {
    assert_eq!(2, 2);
}
"""
A_IGNORED = A_BASE.replace("#[test]\nfn one()", "#[test]\n#[ignore]\nfn one()", 1)
A_PLUS_TEST = A_BASE + """
#[test]
fn three() {
    assert_eq!(3, 3);
}
"""
A_PLUS_TEST_IGNORED = A_PLUS_TEST.replace(
    "#[test]\nfn one()", "#[test]\n#[ignore]\nfn one()", 1)

# Macro-template file, as in the observed blastguard case.
M_BASE = """\
macro_rules! t {
    ($name:ident, $v:expr) => {
        #[test]
        fn $name() {
            assert!($v > 0);
        }
    };
}

t!(a, 1);
t!(b, 2);
"""
# A change that is not one of the proven-safe shapes (an invocation removed).
M_UNPROVEN = M_BASE.replace("t!(b, 2);\n", "")

B_BASE = "notes\n"
B_EDIT = "notes\nmore\n"


class IndexNotWorktree(unittest.TestCase):
    def setUp(self) -> None:
        td = tempfile.TemporaryDirectory(prefix="ctwindex.")
        self.addCleanup(td.cleanup)
        self.tmp = Path(os.path.realpath(td.name))
        home = self.tmp / "home"
        home.mkdir()
        self.env = {
            "GIT_AUTHOR_NAME": "t", "GIT_AUTHOR_EMAIL": "t@example.invalid",
            "GIT_COMMITTER_NAME": "t", "GIT_COMMITTER_EMAIL": "t@example.invalid",
            "GIT_CONFIG_GLOBAL": os.devnull, "GIT_CONFIG_NOSYSTEM": "1",
            "HOME": str(home), "TMPDIR": str(self.tmp), "LC_ALL": "C",
            "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
        }
        self.repo = self.tmp / "repo"
        (self.repo / "crates/c/tests").mkdir(parents=True)

    def git(self, *args: str) -> str:
        return subprocess.run(["git", *args], cwd=self.repo, env=self.env, check=True,
                              capture_output=True, text=True).stdout

    def write(self, rel: str, src: str) -> None:
        (self.repo / rel).write_text(src)

    def base(self, a_src: str = A_BASE) -> None:
        self.git("init", "-q", "-b", "main")
        self.write(A, a_src)
        self.write(B, B_BASE)
        self.git("add", "-A")
        self.git("-c", "core.hooksPath=/dev/null", "-c", "commit.gpgsign=false",
                 "commit", "-q", "-m", "C0 on main")
        self.git("checkout", "-q", "-b", "feat")

    def staged(self, rel: str) -> str:
        return self.git("show", f":{rel}")

    def gate(self) -> tuple[int, str]:
        """Run the gate the way .githooks/commit-msg does."""
        msg = self.tmp / "COMMIT_EDITMSG"
        msg.write_text("commit\n")
        r = subprocess.run([sys.executable, str(SCRIPT), "--repo", str(self.repo),
                            "--pending-msg", str(msg)],
                           cwd=self.repo, env=self.env, capture_output=True,
                           text=True, timeout=60)
        return r.returncode, f"rc={r.returncode} stdout={r.stdout!r} stderr={r.stderr!r}"

    # -- RED: unstaged weakening must not be judged --------------------------

    def test_unstaged_ignore_in_a_with_only_b_staged_is_not_reported(self) -> None:
        self.base()
        self.write(B, B_EDIT)
        self.git("add", B)
        self.write(A, A_IGNORED)  # unstaged
        self.assertEqual(self.staged(A), A_BASE, "anti-vacuity: A's index is untouched")
        self.assertIn(A, self.git("diff", "--name-only"), "anti-vacuity: A is unstaged")
        rc, out = self.gate()
        self.assertNotIn(A, out, "an unstaged change is not part of the commit: " + out)
        self.assertEqual(rc, 0, out)

    def test_unstaged_unproven_macro_change_with_only_b_staged_is_not_undetermined(self) -> None:
        self.base(M_BASE)
        self.write(B, B_EDIT)
        self.git("add", B)
        self.write(A, M_UNPROVEN)  # unstaged
        self.assertEqual(self.staged(A), M_BASE, "anti-vacuity: A's index is untouched")
        rc, out = self.gate()
        self.assertNotIn(A, out, "an unstaged change is not part of the commit: " + out)
        self.assertEqual(rc, 0, out)

    # -- RED: staged weakening hidden by the working tree must block ---------

    def test_staged_ignore_with_worktree_restored_blocks(self) -> None:
        self.base()
        self.write(A, A_IGNORED)
        self.git("add", A)
        self.write(A, A_BASE)  # worktree back to the original
        self.assertEqual(self.staged(A), A_IGNORED, "anti-vacuity: the index is weakened")
        rc, out = self.gate()
        self.assertEqual(rc, 1, "the commit adds #[ignore] but the gate passed: " + out)
        self.assertIn("ignore-added", out)

    def test_partial_staging_index_weakened_worktree_differs_blocks(self) -> None:
        self.base()
        self.write(A, A_IGNORED)
        self.git("add", A)
        self.write(A, A_PLUS_TEST)  # differs from both HEAD and the index
        idx = self.staged(A)
        self.assertNotEqual(idx, A_BASE, "anti-vacuity: index differs from HEAD")
        self.assertNotEqual(idx, A_PLUS_TEST, "anti-vacuity: index differs from worktree")
        rc, out = self.gate()
        self.assertEqual(rc, 1, "the commit adds #[ignore] but the gate passed: " + out)
        self.assertIn("ignore-added", out)

    def test_partial_staging_index_harmless_worktree_weakened_is_clean(self) -> None:
        self.base()
        self.write(A, A_PLUS_TEST)
        self.git("add", A)
        self.write(A, A_PLUS_TEST_IGNORED)  # unstaged #[ignore] on top
        idx = self.staged(A)
        self.assertNotEqual(idx, A_BASE, "anti-vacuity: index differs from HEAD")
        self.assertNotEqual(idx, A_PLUS_TEST_IGNORED,
                            "anti-vacuity: index differs from worktree")
        rc, out = self.gate()
        self.assertNotIn("ignore-added", out, "the #[ignore] is not staged: " + out)
        self.assertEqual(rc, 0, out)

    # -- CONTROLS (pass today) ------------------------------------------------

    def test_control_staged_and_worktree_ignore_blocks(self) -> None:
        self.base()
        self.write(A, A_IGNORED)
        self.git("add", A)
        rc, out = self.gate()
        self.assertEqual(rc, 1, out)
        self.assertIn("ignore-added", out)

    def test_control_b_staged_alone_is_clean(self) -> None:
        self.base()
        self.write(B, B_EDIT)
        self.git("add", B)
        rc, out = self.gate()
        self.assertEqual(rc, 0, out)


if __name__ == "__main__":
    unittest.main()
