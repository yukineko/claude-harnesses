#!/usr/bin/env python3
"""Merge-commit tests for scripts/check-file-growth.py.

Defect (observed 2026-11-04): the staged-baseline rule compares only against
first-parent HEAD. When a merge brings in the OTHER parent's baseline and the
oversized files it grandfathers (both byte-for-byte), the gate exits 1 with
"new entry ... does not exist at HEAD" / "exceeds its N lines at HEAD", even
though the merge result introduces no growth relative to the parent it came
from.

Contract encoded here (each test builds a throwaway repo with a REAL
`git merge` so MERGE_HEAD exists, then runs the real script as a subprocess):
  A. other parent's baseline lists `N path`, other parent's file has N > LIMIT
     lines, HEAD has neither; staged = other parent's file + baseline -> 0
  B. same, but HEAD has the file at fewer lines and no entry -> 0
  C. (control) staged cap higher than the cap in every parent's baseline -> 1
  D. (control) new entry > LIMIT that no parent's baseline lists and that
     exceeds HEAD's line count -> 1 (a merge must not grandfather growth)
  E. (control) no merge in progress: new entry absent at HEAD -> 1 (unchanged)

Run: python3 -m pytest scripts/test_check_file_growth_merge.py -q
"""
import subprocess
import tempfile
import unittest
from pathlib import Path

_HERE = Path(__file__).resolve().parent
SCRIPT = _HERE / "check-file-growth.py"
BASELINE = "scripts/check-file-growth.baseline"
LIMIT = 2000


def _git(repo, *args, check=True):
    return subprocess.run(
        ["git", "-C", str(repo), *args], capture_output=True, text=True, check=check
    )


def _lines(n, tag="x"):
    return "".join(f"{tag} = {i}\n" for i in range(n))


class _Repo:
    def __init__(self, tmp):
        base = Path(tmp)
        self.root = base / "repo"
        self.root.mkdir()
        hooks = base / "empty-hooks"
        hooks.mkdir()
        _git(self.root, "init", "-q", "-b", "main")
        _git(self.root, "config", "user.email", "t@example.com")
        _git(self.root, "config", "user.name", "t")
        _git(self.root, "config", "commit.gpgsign", "false")
        # Do not inherit the user's hooksPath (this repo's own gates).
        _git(self.root, "config", "core.hooksPath", str(hooks))

    def write(self, rel, text):
        p = self.root / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(text)

    def stage(self, *rels):
        _git(self.root, "add", "--", *rels)

    def commit(self, msg="c"):
        _git(self.root, "commit", "-q", "-m", msg)

    def checkout(self, *args):
        _git(self.root, "checkout", "-q", *args)

    def merge_head_exists(self):
        r = _git(self.root, "rev-parse", "-q", "--verify", "MERGE_HEAD", check=False)
        return r.returncode == 0

    def run(self):
        assert SCRIPT.is_file(), f"{SCRIPT} does not exist"
        return subprocess.run(
            ["python3", str(SCRIPT)], cwd=self.root, capture_output=True, text=True
        )


class MergeBaseline(unittest.TestCase):
    def setUp(self):
        self._td = tempfile.TemporaryDirectory()
        self.addCleanup(self._td.cleanup)
        self.repo = _Repo(self._td.name)
        # Common ancestor.
        self.repo.write("seed.txt", "seed\n")
        self.repo.stage("seed.txt")
        self.repo.commit("seed")

    def _branch_other(self, files):
        """Create branch `other` from the seed commit with `files`, then
        return to main."""
        self.repo.checkout("-b", "other")
        for rel, text in files.items():
            self.repo.write(rel, text)
        self.repo.stage(*files)
        self.repo.commit("other side")
        self.repo.checkout("main")

    def _main_side(self, files):
        for rel, text in files.items():
            self.repo.write(rel, text)
        self.repo.stage(*files)
        self.repo.commit("main side")

    def _merge_with_conflict_on_seed(self):
        """Run a real `git merge other` that conflicts on seed.txt only;
        resolve that conflict and stage it. MERGE_HEAD stays set."""
        r = _git(self.repo.root, "merge", "--no-ff", "other", check=False)
        self.assertNotEqual(r.returncode, 0, "expected a conflict: " + r.stdout + r.stderr)
        self.repo.write("seed.txt", "resolved\n")
        self.repo.stage("seed.txt")
        self.assertTrue(self.repo.merge_head_exists())

    def _assert_index_matches_other(self, *paths):
        for p in paths:
            staged = _git(self.repo.root, "rev-parse", f":{p}").stdout.strip()
            theirs = _git(self.repo.root, "rev-parse", f"other:{p}").stdout.strip()
            self.assertEqual(staged, theirs, f"{p} staged != other:{p}")

    # ---- RED: merging in the other parent's grandfathered state ----

    def test_A_merge_brings_other_parents_baseline_and_file_absent_at_head(self):
        self._branch_other({
            "seed.txt": "theirs\n",
            "crates/blastguard/src/detect/tests.rs": _lines(5021),
            BASELINE: "5021 crates/blastguard/src/detect/tests.rs\n",
        })
        self._main_side({"seed.txt": "ours\n"})
        self._merge_with_conflict_on_seed()
        self._assert_index_matches_other(
            "crates/blastguard/src/detect/tests.rs", BASELINE
        )
        r = self.repo.run()
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)

    def test_A2_no_commit_merge_without_conflict(self):
        self._branch_other({
            "big.rs": _lines(2500),
            BASELINE: "2500 big.rs\n",
        })
        self._main_side({"ours.txt": "ours\n"})
        _git(self.repo.root, "merge", "--no-ff", "--no-commit", "other")
        self.assertTrue(self.repo.merge_head_exists())
        self._assert_index_matches_other("big.rs", BASELINE)
        r = self.repo.run()
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)

    def test_B_merge_takes_other_parents_bigger_file_present_smaller_at_head(self):
        self._branch_other({
            "seed.txt": "theirs\n",
            "big.rs": _lines(2480, "theirs"),
            BASELINE: "2480 big.rs\n",
        })
        self._main_side({"seed.txt": "ours\n", "big.rs": _lines(2249, "ours")})
        r = _git(self.repo.root, "merge", "--no-ff", "other", check=False)
        self.assertNotEqual(r.returncode, 0, r.stdout + r.stderr)
        self.repo.write("seed.txt", "resolved\n")
        _git(self.repo.root, "checkout", "--theirs", "--", "big.rs")
        self.repo.stage("seed.txt", "big.rs")
        self.assertTrue(self.repo.merge_head_exists())
        self._assert_index_matches_other("big.rs", BASELINE)
        r = self.repo.run()
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)

    # ---- CONTROLS: must stay violations ----

    def test_C_merge_staged_cap_above_every_parent_is_violation(self):
        self._branch_other({
            "seed.txt": "theirs\n",
            "big.rs": _lines(2500),
            BASELINE: "2500 big.rs\n",
        })
        self._main_side({"seed.txt": "ours\n"})
        self._merge_with_conflict_on_seed()
        self.repo.write("big.rs", _lines(2600))
        self.repo.write(BASELINE, "2600 big.rs\n")
        self.repo.stage("big.rs", BASELINE)
        r = self.repo.run()
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        self.assertIn("big.rs", r.stdout + r.stderr)

    def test_D_merge_new_entry_listed_by_no_parent_is_violation(self):
        self._branch_other({
            "seed.txt": "theirs\n",
            "big.rs": _lines(2500),
            BASELINE: "2500 big.rs\n",
        })
        self._main_side({"seed.txt": "ours\n", "rogue.rs": _lines(100)})
        self._merge_with_conflict_on_seed()
        self.repo.write("rogue.rs", _lines(2100))
        self.repo.write(BASELINE, "2500 big.rs\n2100 rogue.rs\n")
        self.repo.stage("rogue.rs", BASELINE)
        r = self.repo.run()
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        self.assertIn("rogue.rs", r.stdout + r.stderr)

    def test_D2_merge_new_entry_absent_everywhere_is_violation(self):
        self._branch_other({
            "seed.txt": "theirs\n",
            "big.rs": _lines(2500),
            BASELINE: "2500 big.rs\n",
        })
        self._main_side({"seed.txt": "ours\n"})
        self._merge_with_conflict_on_seed()
        self.repo.write("rogue.rs", _lines(2100))
        self.repo.write(BASELINE, "2500 big.rs\n2100 rogue.rs\n")
        self.repo.stage("rogue.rs", BASELINE)
        r = self.repo.run()
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        self.assertIn("rogue.rs", r.stdout + r.stderr)

    def test_E_no_merge_new_entry_absent_at_head_is_violation(self):
        self.assertFalse(self.repo.merge_head_exists())
        self.repo.write("big.rs", _lines(2500))
        self.repo.write(BASELINE, "2500 big.rs\n")
        self.repo.stage("big.rs", BASELINE)
        r = self.repo.run()
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        self.assertIn("does not exist at HEAD", r.stdout + r.stderr)


if __name__ == "__main__":
    unittest.main(verbosity=1)
