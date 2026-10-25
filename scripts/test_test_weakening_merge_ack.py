#!/usr/bin/env python3
"""check-test-weakening.py during a merge of main INTO a feature branch.

Observed 2026-10-16: merging main into a feature branch was blocked by the
commit-msg hook with an `assertion-removed` finding that came from MAIN's side
(crates/fugu-router/src/budget.rs, parses_pressure_false 2 -> 1, from 96da33f6),
although that main commit carries the matching `test-weakening-justified:` line.

Hypothesis under test: the scanner diffs merge-base(main, HEAD) against the
WORKTREE (which, mid-merge, contains main's change) but reads acknowledgments
only from `merge-base..HEAD` plus the pending message. Main's commits are
reachable only through MERGE_HEAD, so main's own ack is invisible and the merge
is charged with main's already-acknowledged change.

Claimed property: an assertion removal that main already acknowledged in its
own commit message does not block the merge commit that brings main into a
feature branch with a plain merge message.

The scanner is invoked exactly as .githooks/commit-msg invokes it
(`python3 <scanner> --repo <repo> --pending-msg <msgfile>`, no --base: the
default base is local main, per the #1181 ruling, with no origin/main fallback),
with a real `git merge --no-commit` in progress (MERGE_HEAD present).

RED repro:  python3 -m pytest --runxfail scripts/test_test_weakening_merge_ack.py
"""
from __future__ import annotations

import os
import subprocess
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent / "check-test-weakening.py"

GIT_ENV = {
    "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
    "GIT_AUTHOR_NAME": "Test Author",
    "GIT_AUTHOR_EMAIL": "test@example.invalid",
    "GIT_COMMITTER_NAME": "Test Author",
    "GIT_COMMITTER_EMAIL": "test@example.invalid",
    "GIT_CONFIG_GLOBAL": os.devnull,
    "GIT_CONFIG_SYSTEM": os.devnull,
    "GIT_CONFIG_NOSYSTEM": "1",
    "HOME": "/nonexistent-home-for-tests",
    "LC_ALL": "C",
}

TEST_PATH = "crates/demo/tests/arith.rs"
KIND = "assertion-removed"
ACK = f"test-weakening-justified: {TEST_PATH}:{KIND} - the removed assert pinned the old contract"

TESTS_BASE = """\
#[test]
fn adds() {
    assert_eq!(1 + 1, 2);
    assert_eq!(2 + 2, 4);
}
"""

TESTS_WEAKENED = """\
#[test]
fn adds() {
    assert_eq!(1 + 1, 2);
}
"""


class Repo:
    def __init__(self, root: Path) -> None:
        self.root = root

    def git(self, *args: str, check: bool = True) -> subprocess.CompletedProcess:
        return subprocess.run(["git", *args], cwd=str(self.root), env=GIT_ENV,
                              capture_output=True, text=True, check=check)

    def write(self, rel: str, content: str) -> None:
        p = self.root / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(content, encoding="utf-8")

    def commit(self, message: str) -> None:
        self.git("add", "-A")
        self.git("commit", "-q", "-m", message)

    def scan_like_commit_msg_hook(self, message: str) -> tuple[int, str]:
        msg = self.root.parent / "COMMIT_EDITMSG"
        msg.write_text(message + "\n", encoding="utf-8")
        proc = subprocess.run(
            ["python3", str(SCRIPT), "--repo", str(self.root), "--pending-msg", str(msg)],
            capture_output=True, text=True, env=GIT_ENV,
        )
        return proc.returncode, proc.stdout + proc.stderr


class MergeMainIntoFeature(unittest.TestCase):
    def make(self) -> Repo:
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        root = Path(tmp.name) / "repo"
        root.mkdir()
        r = Repo(root)
        r.git("init", "-q", "-b", "main")
        r.write(TEST_PATH, TESTS_BASE)
        r.write("README", "base\n")
        r.commit("base")
        r.git("branch", "feature")  # forked before main's change
        return r

    def merge_main_into_feature(self, r: Repo) -> None:
        r.git("checkout", "-q", "feature")
        r.git("merge", "--no-commit", "--no-ff", "main")
        self.assertEqual(r.git("rev-parse", "-q", "--verify", "MERGE_HEAD", check=False).returncode, 0,
                         "fixture: MERGE_HEAD must be present (merge in progress)")
        self.assertEqual((r.root / TEST_PATH).read_text(), TESTS_WEAKENED,
                         "fixture: main's removal must be in the worktree")

    def feature_unrelated_commit(self, r: Repo) -> None:
        r.git("checkout", "-q", "feature")
        r.write("README", "feature work\n")
        r.commit("feature: unrelated change")

    def test_main_side_acked_removal_does_not_block_the_merge(self) -> None:
        r = self.make()
        r.write(TEST_PATH, TESTS_WEAKENED)
        r.commit("main: drop obsolete assert\n\n" + ACK)
        self.feature_unrelated_commit(r)
        self.merge_main_into_feature(r)
        rc, out = r.scan_like_commit_msg_hook("Merge branch 'main' into feature")
        self.assertEqual(rc, 0, f"merge of main's ACKED removal was blocked:\n{out}")

    def test_control_main_side_unacked_removal_still_blocks_the_merge(self) -> None:
        """Today's behaviour, pinned: an unacknowledged removal arriving from
        main is charged to the merge and blocks it."""
        r = self.make()
        r.write(TEST_PATH, TESTS_WEAKENED)
        r.commit("main: drop assert, no ack")
        self.feature_unrelated_commit(r)
        self.merge_main_into_feature(r)
        rc, out = r.scan_like_commit_msg_hook("Merge branch 'main' into feature")
        self.assertEqual(rc, 1, out)
        self.assertIn(f"[BLOCK] {TEST_PATH}: {KIND}", out)

    def test_control_feature_side_unacked_removal_blocks(self) -> None:
        r = self.make()
        r.git("checkout", "-q", "feature")
        r.write(TEST_PATH, TESTS_WEAKENED)
        r.commit("feature: drop assert, no ack")
        rc, out = r.scan_like_commit_msg_hook("feature: follow-up")
        self.assertEqual(rc, 1, out)
        self.assertIn(f"[BLOCK] {TEST_PATH}: {KIND}", out)

    def test_control_same_ack_on_feature_side_is_honoured(self) -> None:
        """Anti-vacuity: the ACK line is well-formed — the identical removal and
        ack on the feature branch's own commit scan clean, so a failure of the
        expected-failure case is about WHERE the ack lives, not its spelling."""
        r = self.make()
        r.git("checkout", "-q", "feature")
        r.write(TEST_PATH, TESTS_WEAKENED)
        r.commit("feature: drop obsolete assert\n\n" + ACK)
        rc, out = r.scan_like_commit_msg_hook("feature: follow-up")
        self.assertEqual(rc, 0, out)
        self.assertIn(f"[ack] {TEST_PATH}: {KIND}", out)

    def test_unresolvable_merge_head_contributes_no_acks(self) -> None:
        """A MERGE_HEAD file that is present but does not resolve to a commit
        (well-formed sha of a nonexistent object) contributes NO acks: main's
        removal, acknowledged only in a commit no longer reachable from HEAD or
        a resolvable MERGE_HEAD, still blocks the merge.

        Anti-vacuity contrast: the identical scenario with an intact MERGE_HEAD
        passes (test_main_side_acked_removal_does_not_block_the_merge), so the
        block here is caused by the unresolvable MERGE_HEAD, not the fixture."""
        r = self.make()
        r.write(TEST_PATH, TESTS_WEAKENED)
        r.commit("main: drop obsolete assert\n\n" + ACK)
        self.feature_unrelated_commit(r)
        self.merge_main_into_feature(r)

        git_path = r.git("rev-parse", "--git-path", "MERGE_HEAD").stdout.strip()
        merge_head_file = Path(git_path)
        if not merge_head_file.is_absolute():
            merge_head_file = r.root / merge_head_file
        self.assertTrue(merge_head_file.is_file(), "fixture: MERGE_HEAD file must exist before corruption")
        merge_head_file.write_text("d" * 40 + "\n", encoding="utf-8")

        self.assertTrue(merge_head_file.is_file(), "fixture: MERGE_HEAD file must still be PRESENT")
        self.assertNotEqual(
            r.git("rev-parse", "--verify", "--quiet", "MERGE_HEAD^{commit}", check=False).returncode, 0,
            "fixture: MERGE_HEAD must NOT resolve to a commit")
        self.assertEqual((r.root / TEST_PATH).read_text(), TESTS_WEAKENED,
                         "fixture: main's removal must be in the worktree")
        log_head = r.git("log", "--format=%B", "HEAD").stdout
        self.assertNotIn(ACK, log_head, "fixture: the ACK must not be reachable from HEAD")

        rc, out = r.scan_like_commit_msg_hook("Merge branch 'main' into feature")
        self.assertEqual(rc, 1, f"unresolvable MERGE_HEAD let the removal through:\n{out}")
        self.assertIn(f"[BLOCK] {TEST_PATH}: {KIND}", out)


if __name__ == "__main__":
    unittest.main()
