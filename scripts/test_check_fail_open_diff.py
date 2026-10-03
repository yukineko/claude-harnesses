#!/usr/bin/env python3
"""Unit tests for scripts/check-fail-open-diff.py (fail-open diff ratchet).

Stdlib-only; run with  `python3 scripts/test_check_fail_open_diff.py`.

Load-bearing properties (backlog 2133b6fe, option (b) chosen by the user):
  1. A swallow NEWLY introduced into a staged file is caught (exit 1).
  2. A pre-existing swallow in a file that is not changed — or is changed
     without adding one — does not fire (exit 0). This is the property that
     lets concurrent sessions commit without being blocked by each other.
  3. Cannot-determine (no HEAD, git failure) resolves to exit 2, never 0.
  4. The staged (index) content is what is judged, not the working tree.
"""

from __future__ import annotations

import importlib.util
import os
import subprocess
import tempfile
import unittest
from pathlib import Path

_HERE = Path(__file__).resolve().parent
_SPEC = importlib.util.spec_from_file_location(
    "check_fail_open_diff", _HERE / "check-fail-open-diff.py"
)
fd = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(fd)

SWALLOW = "        Err(_) => Vec::new(),\n"
CLEAN_RS = (
    "pub fn f() -> Vec<u8> {\n"
    "    match g() {\n"
    "        Ok(v) => v,\n"
    "        Err(e) => panic!(\"{e}\"),\n"
    "    }\n"
    "}\n"
)
OLD_SWALLOW_RS = (
    "pub fn old() -> Vec<u8> {\n"
    "    match g() {\n"
    "        Ok(v) => v,\n"
    + SWALLOW
    + "    }\n"
    "}\n"
)


def git(repo: Path, *args: str) -> str:
    return subprocess.run(
        ["git", "-C", str(repo), *args],
        capture_output=True, text=True, check=True,
        env={**os.environ, "GIT_CONFIG_GLOBAL": os.devnull,
             "GIT_CONFIG_NOSYSTEM": "1"},
    ).stdout


class Repo:
    def __init__(self) -> None:
        self._tmp = tempfile.TemporaryDirectory(prefix="fo-diff-")
        self.path = Path(self._tmp.name)
        git(self.path, "init", "-q", "-b", "main")
        git(self.path, "config", "user.email", "t@example.invalid")
        git(self.path, "config", "user.name", "t")

    def write(self, rel: str, text: str) -> None:
        p = self.path / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(text, encoding="utf-8")

    def stage(self, *rels: str) -> None:
        git(self.path, "add", "--", *rels)

    def commit(self) -> None:
        git(self.path, "commit", "-q", "-m", "c", "--no-verify")

    def close(self) -> None:
        self._tmp.cleanup()


class DiffRatchet(unittest.TestCase):
    def setUp(self) -> None:
        self.repo = Repo()
        # Baseline: one file that ALREADY holds a swallow, one clean file.
        self.repo.write("crates/a/src/old.rs", OLD_SWALLOW_RS)
        self.repo.write("crates/a/src/lib.rs", CLEAN_RS)
        self.repo.stage("crates")
        self.repo.commit()

    def tearDown(self) -> None:
        self.repo.close()

    def test_new_swallow_in_changed_file_blocks(self):
        self.repo.write("crates/a/src/lib.rs", CLEAN_RS.replace(
            "        Err(e) => panic!(\"{e}\"),\n", SWALLOW))
        self.repo.stage("crates/a/src/lib.rs")
        code, rises = fd.evaluate(self.repo.path)
        self.assertEqual(code, 1)
        self.assertEqual([r.path for r in rises], ["crates/a/src/lib.rs"])

    def test_new_file_with_a_swallow_blocks(self):
        self.repo.write("crates/a/src/new.rs", OLD_SWALLOW_RS)
        self.repo.stage("crates/a/src/new.rs")
        code, rises = fd.evaluate(self.repo.path)
        self.assertEqual(code, 1)
        self.assertEqual([r.path for r in rises], ["crates/a/src/new.rs"])

    def test_preexisting_swallow_in_unchanged_file_does_not_fire(self):
        # A change elsewhere; old.rs keeps its pre-existing swallow.
        self.repo.write("crates/a/src/lib.rs", CLEAN_RS + "// touched\n")
        self.repo.stage("crates/a/src/lib.rs")
        code, rises = fd.evaluate(self.repo.path)
        self.assertEqual((code, rises), (0, []))

    def test_editing_a_file_that_keeps_its_old_swallow_does_not_fire(self):
        self.repo.write("crates/a/src/old.rs", "// header\n" + OLD_SWALLOW_RS)
        self.repo.stage("crates/a/src/old.rs")
        code, rises = fd.evaluate(self.repo.path)
        self.assertEqual((code, rises), (0, []))

    def test_removing_a_swallow_passes(self):
        self.repo.write("crates/a/src/old.rs", CLEAN_RS)
        self.repo.stage("crates/a/src/old.rs")
        code, rises = fd.evaluate(self.repo.path)
        self.assertEqual((code, rises), (0, []))

    def test_unstaged_swallow_is_not_judged_staged_content_is(self):
        # Working tree gains a swallow but only a clean edit is staged: the
        # commit does not contain the swallow, so it must not block...
        self.repo.write("crates/a/src/lib.rs", CLEAN_RS + "// staged\n")
        self.repo.stage("crates/a/src/lib.rs")
        self.repo.write("crates/a/src/lib.rs", CLEAN_RS.replace(
            "        Err(e) => panic!(\"{e}\"),\n", SWALLOW))
        self.assertEqual(fd.evaluate(self.repo.path)[0], 0)
        # ...and the reverse: a staged swallow hidden by a clean working tree
        # must still block.
        self.repo.stage("crates/a/src/lib.rs")
        self.repo.write("crates/a/src/lib.rs", CLEAN_RS)
        self.assertEqual(fd.evaluate(self.repo.path)[0], 1)

    def test_new_blocked_arm_empty_fallback_blocks(self):
        self.repo.write("crates/a/src/lib.rs", CLEAN_RS.replace(
            "        Err(e) => panic!(\"{e}\"),\n",
            "        Required::Blocked(_) => Vec::new(),\n"))
        self.repo.stage("crates/a/src/lib.rs")
        code, rises = fd.evaluate(self.repo.path)
        self.assertEqual(code, 1)
        self.assertEqual([r.path for r in rises], ["crates/a/src/lib.rs"])

    def test_out_of_scope_file_is_ignored(self):
        self.repo.write("docs/x.rs", OLD_SWALLOW_RS)
        self.repo.stage("docs/x.rs")
        self.assertEqual(fd.evaluate(self.repo.path), (0, []))

    def test_main_exit_codes(self):
        self.assertEqual(fd.main(["x", "--repo", str(self.repo.path)]), 0)
        self.repo.write("crates/a/src/new.rs", OLD_SWALLOW_RS)
        self.repo.stage("crates/a/src/new.rs")
        self.assertEqual(fd.main(["x", "--repo", str(self.repo.path)]), 1)


class MergeAttribution(unittest.TestCase):
    """A merge commit is compared against EVERY parent, not only HEAD.

    Measured 2026-10-03 (session-64554c4d merging origin/main b0f626e4):
    crates/jev/src/client.rs exists only on the MERGE_HEAD side, so against
    HEAD it is a brand-new file and its pre-existing hit was attributed to
    the merge. CLAUDE.md §8 names that mis-attribution a bug. A hit is the
    merge's own only when the merged blob holds more hits than EVERY parent's
    blob of that path does.
    """

    def setUp(self) -> None:
        self.repo = Repo()
        self.repo.write("crates/a/src/lib.rs", CLEAN_RS)
        self.repo.stage("crates")
        self.repo.commit()
        git(self.repo.path, "checkout", "-q", "-b", "other")
        self.repo.write("crates/b/src/theirs.rs", OLD_SWALLOW_RS)
        self.repo.stage("crates")
        self.repo.commit()
        git(self.repo.path, "checkout", "-q", "main")
        self.repo.write("crates/a/src/ours.rs", CLEAN_RS)
        self.repo.stage("crates")
        self.repo.commit()

    def tearDown(self) -> None:
        self.repo.close()

    def _merge(self) -> None:
        subprocess.run(
            ["git", "-C", str(self.repo.path), "merge", "-q", "--no-ff",
             "--no-commit", "other"],
            capture_output=True, text=True, check=True,
            env={**os.environ, "GIT_CONFIG_GLOBAL": os.devnull,
                 "GIT_CONFIG_NOSYSTEM": "1"},
        )

    def test_hit_carried_in_from_the_other_parent_does_not_fire(self):
        self._merge()
        self.assertEqual(fd.evaluate(self.repo.path), (0, []))

    def test_swallow_added_by_the_merge_resolution_still_blocks(self):
        self._merge()
        self.repo.write("crates/a/src/lib.rs", CLEAN_RS.replace(
            "        Err(e) => panic!(\"{e}\"),\n", SWALLOW))
        self.repo.stage("crates/a/src/lib.rs")
        code, rises = fd.evaluate(self.repo.path)
        self.assertEqual(code, 1)
        self.assertEqual([r.path for r in rises], ["crates/a/src/lib.rs"])

    def test_union_of_both_parents_hits_in_one_file_blocks(self):
        # Each parent's blob holds ONE hit; the merged blob holds two. That
        # exceeds every parent, so it is the merge's own and must block.
        git(self.repo.path, "checkout", "-q", "other")
        self.repo.write("crates/a/src/lib.rs", OLD_SWALLOW_RS)
        self.repo.stage("crates")
        self.repo.commit()
        git(self.repo.path, "checkout", "-q", "main")
        self.repo.write("crates/a/src/lib.rs", OLD_SWALLOW_RS.replace(
            "pub fn old()", "pub fn mine()"))
        self.repo.stage("crates")
        self.repo.commit()
        subprocess.run(
            ["git", "-C", str(self.repo.path), "merge", "-q", "--no-ff",
             "--no-commit", "other"],
            capture_output=True, text=True,
            env={**os.environ, "GIT_CONFIG_GLOBAL": os.devnull,
                 "GIT_CONFIG_NOSYSTEM": "1"},
        )
        self.repo.write("crates/a/src/lib.rs",
                        OLD_SWALLOW_RS + OLD_SWALLOW_RS.replace(
                            "pub fn old()", "pub fn mine()"))
        self.repo.stage("crates/a/src/lib.rs")
        code, rises = fd.evaluate(self.repo.path)
        self.assertEqual(code, 1)
        self.assertIn("crates/a/src/lib.rs", [r.path for r in rises])

    # Round-2 verifier of 9d16cecd: git records only the INDEPENDENT parents.
    # A MERGE_HEAD naming an ancestor of HEAD yields a single-parent commit on
    # HEAD; one naming a descendant yields a single-parent commit on THAT
    # commit. The gate must judge against the parents git will record.
    def _git_dir(self) -> Path:
        return self.repo.path / git(self.repo.path, "rev-parse",
                                    "--git-dir").strip()

    def test_ancestor_merge_head_cannot_launder_a_readded_swallow(self):
        git(self.repo.path, "checkout", "-q", "-b", "fixline")
        self.repo.write("crates/a/src/lib.rs", OLD_SWALLOW_RS)
        self.repo.stage("crates")
        self.repo.commit()
        old = git(self.repo.path, "rev-parse", "HEAD").strip()
        self.repo.write("crates/a/src/lib.rs", CLEAN_RS)
        self.repo.stage("crates")
        self.repo.commit()
        (self._git_dir() / "MERGE_HEAD").write_text(old + "\n",
                                                    encoding="utf-8")
        self.repo.write("crates/a/src/lib.rs", OLD_SWALLOW_RS)
        self.repo.stage("crates/a/src/lib.rs")
        code, rises = fd.evaluate(self.repo.path)
        self.assertEqual(code, 1)
        self.assertEqual([r.path for r in rises], ["crates/a/src/lib.rs"])

    def test_descendant_merge_head_judges_against_the_recorded_parent(self):
        # HEAD holds a swallow; S (a descendant) removed it. With MERGE_HEAD=S
        # git records S as the only parent, so keeping HEAD's blob ADDS the
        # swallow even though `diff --cached HEAD` does not list the file.
        self.repo.write("crates/a/src/lib.rs", OLD_SWALLOW_RS)
        self.repo.stage("crates")
        self.repo.commit()
        head = git(self.repo.path, "rev-parse", "HEAD").strip()
        self.repo.write("crates/a/src/lib.rs", CLEAN_RS)
        self.repo.stage("crates")
        self.repo.commit()
        s = git(self.repo.path, "rev-parse", "HEAD").strip()
        git(self.repo.path, "reset", "-q", "--hard", head)
        (self._git_dir() / "MERGE_HEAD").write_text(s + "\n",
                                                    encoding="utf-8")
        code, rises = fd.evaluate(self.repo.path)
        self.assertEqual(code, 1)
        self.assertEqual([r.path for r in rises], ["crates/a/src/lib.rs"])

    def test_empty_merge_head_is_undetermined(self):
        self._merge()
        (self._git_dir() / "MERGE_HEAD").write_text("", encoding="utf-8")
        with self.assertRaises(fd.Undetermined):
            fd.evaluate(self.repo.path)

    def test_hit_kept_from_our_side_of_a_real_merge_does_not_fire(self):
        # Pins the max: our parent holds the hit, theirs does not.
        self.repo.write("crates/a/src/ours.rs", OLD_SWALLOW_RS)
        self.repo.stage("crates")
        self.repo.commit()
        self._merge()
        self.assertEqual(fd.evaluate(self.repo.path), (0, []))

    def test_unresolvable_merge_head_line_is_undetermined(self):
        self._merge()
        git_dir = git(self.repo.path, "rev-parse", "--git-dir").strip()
        (self.repo.path / git_dir / "MERGE_HEAD").write_text(
            "not-a-commit\n", encoding="utf-8")
        with self.assertRaises(fd.Undetermined):
            fd.evaluate(self.repo.path)


class GitFailureIsUndetermined(unittest.TestCase):
    """Round-3 verifier of ee045867: two Undetermined checks survived their
    deletion as mutants (M17, M5), and the unreadable-MERGE_HEAD claim had
    no test that actually made the file unreadable (M16)."""

    def setUp(self) -> None:
        self.repo = Repo()
        self.repo.write("crates/a/src/lib.rs", CLEAN_RS)
        self.repo.stage("crates")
        self.repo.commit()
        self._real_git = fd._git

    def tearDown(self) -> None:
        fd._git = self._real_git
        self.repo.close()

    def _fail_on(self, subcommand, stdout="", rc=1):
        real = self._real_git

        def fake(repo, *args):
            if args and args[0] == subcommand:
                return subprocess.CompletedProcess(
                    ["git", *args], rc, stdout=stdout, stderr="injected")
            return real(repo, *args)
        fd._git = fake

    def test_failing_diff_cached_is_undetermined(self):
        self.repo.write("crates/a/src/lib.rs", CLEAN_RS + "// edit\n")
        self.repo.stage("crates/a/src/lib.rs")
        self._fail_on("diff")
        with self.assertRaises(fd.Undetermined):
            fd.evaluate(self.repo.path)

    def test_empty_merge_base_output_is_undetermined(self):
        sha = git(self.repo.path, "rev-parse", "HEAD").strip()
        git_dir = git(self.repo.path, "rev-parse", "--git-dir").strip()
        (self.repo.path / git_dir / "MERGE_HEAD").write_text(
            sha + "\n", encoding="utf-8")
        self._fail_on("merge-base", stdout="", rc=0)
        with self.assertRaises(fd.Undetermined):
            fd.evaluate(self.repo.path)

    @unittest.skipIf(os.geteuid() == 0, "root reads a mode-000 file")
    def test_merge_head_that_cannot_be_read_is_undetermined(self):
        git_dir = git(self.repo.path, "rev-parse", "--git-dir").strip()
        mh = self.repo.path / git_dir / "MERGE_HEAD"
        mh.write_text(git(self.repo.path, "rev-parse", "HEAD"),
                      encoding="utf-8")
        mh.chmod(0)
        try:
            with self.assertRaises(fd.Undetermined):
                fd.evaluate(self.repo.path)
        finally:
            mh.chmod(0o644)


class ObjectTypes(unittest.TestCase):
    """Round-2 worker 別件 of ee045867: a path whose TYPE changes is listed as
    `T`, which ACMR never judged, and `git show <rev>:<gitlink>` prints a
    commit plus its patch, which was scanned as Rust."""

    def setUp(self) -> None:
        self.repo = Repo()
        self.repo.write("crates/a/src/lib.rs", CLEAN_RS)
        self.repo.stage("crates")
        self.repo.commit()

    def tearDown(self) -> None:
        self.repo.close()

    def test_gitlink_replaced_by_a_file_is_undetermined(self):
        sha = git(self.repo.path, "rev-parse", "HEAD").strip()
        git(self.repo.path, "update-index", "--add", "--cacheinfo",
            f"160000,{sha},crates/a/src/sub.rs")
        self.repo.commit()
        git(self.repo.path, "rm", "-q", "--cached", "crates/a/src/sub.rs")
        self.repo.write("crates/a/src/sub.rs", OLD_SWALLOW_RS)
        self.repo.stage("crates/a/src/sub.rs")
        with self.assertRaises(fd.Undetermined):
            fd.evaluate(self.repo.path)

    def test_directory_replaced_by_a_file_with_a_swallow_blocks(self):
        self.repo.write("crates/a/src/d.rs/x.txt", "x\n")
        self.repo.stage("crates")
        self.repo.commit()
        git(self.repo.path, "rm", "-q", "-r", "--cached", "crates/a/src/d.rs")
        (self.repo.path / "crates/a/src/d.rs/x.txt").unlink()
        (self.repo.path / "crates/a/src/d.rs").rmdir()
        self.repo.write("crates/a/src/d.rs", OLD_SWALLOW_RS)
        self.repo.stage("crates/a/src/d.rs")
        code, rises = fd.evaluate(self.repo.path)
        self.assertEqual(code, 1)
        self.assertEqual([r.path for r in rises], ["crates/a/src/d.rs"])


class Undetermined(unittest.TestCase):
    def test_no_head_is_undetermined_not_clean(self):
        repo = Repo()
        try:
            repo.write("crates/a/src/lib.rs", OLD_SWALLOW_RS)
            repo.stage("crates")
            with self.assertRaises(fd.Undetermined):
                fd.evaluate(repo.path)
            self.assertEqual(fd.main(["x", "--repo", str(repo.path)]), 2)
        finally:
            repo.close()

    def test_not_a_git_repo_is_undetermined(self):
        with tempfile.TemporaryDirectory() as d:
            self.assertEqual(fd.main(["x", "--repo", d]), 2)


if __name__ == "__main__":
    unittest.main()
