#!/usr/bin/env python3
"""Tests for scripts/check-file-growth.py (pre-commit file-growth ratchet).

Stdlib-only (`unittest`), no network. Each test builds a throwaway git repo and
runs the REAL script (by absolute path, as a subprocess, cwd = the temp repo)
against STAGED content. Exit codes: 0 pass, 1 violation, 2 undetermined.

Spec encoded here:
  * only staged added/modified `.rs` / `.py` files are checked, by index content
  * LIMIT = 2000 lines; a baseline entry `<max_lines> <path>` replaces the limit
  * the staged baseline may not raise a cap vs HEAD, nor add an entry > LIMIT
  * unstaged working-tree edits are ignored
  * undetermined (unparsable baseline line, git failure) is exit 2, never 0

Run: python3 -m unittest scripts/test_check_file_growth.py
 or: python3 scripts/test_check_file_growth.py
"""
import subprocess
import tempfile
import unittest
from pathlib import Path

_HERE = Path(__file__).resolve().parent
REPO_ROOT = _HERE.parent
SCRIPT = _HERE / "check-file-growth.py"
BASELINE = "scripts/check-file-growth.baseline"
LIMIT = 2000


def _git(repo, *args):
    return subprocess.run(
        ["git", "-C", str(repo), *args], capture_output=True, text=True, check=True
    )


def _lines(n):
    return "".join(f"x = {i}\n" for i in range(n))


class _Repo:
    def __init__(self, tmp):
        self.root = Path(tmp)
        _git(self.root, "init", "-q")
        _git(self.root, "config", "user.email", "t@example.com")
        _git(self.root, "config", "user.name", "t")
        _git(self.root, "config", "commit.gpgsign", "false")

    def write(self, rel, text):
        p = self.root / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(text)

    def stage(self, *rels):
        _git(self.root, "add", "--", *rels)

    def commit(self, msg="c"):
        _git(self.root, "commit", "-q", "-m", msg)

    def run(self):
        # Without this, a missing script makes python exit 2 and the
        # "undetermined" test would pass vacuously.
        assert SCRIPT.is_file(), f"{SCRIPT} does not exist"
        return subprocess.run(
            ["python3", str(SCRIPT)],
            cwd=self.root, capture_output=True, text=True,
        )


class FileGrowth(unittest.TestCase):
    def setUp(self):
        self._td = tempfile.TemporaryDirectory()
        self.addCleanup(self._td.cleanup)
        self.repo = _Repo(self._td.name)

    def _head_with_baseline(self, entries, big_lines=None):
        """Commit a baseline (and optionally big.rs) so HEAD exists."""
        self.repo.write(BASELINE, entries)
        files = [BASELINE]
        if big_lines is not None:
            self.repo.write("big.rs", _lines(big_lines))
            files.append("big.rs")
        self.repo.stage(*files)
        self.repo.commit("base")

    # ---- RED (script missing / behaviour absent) ----

    def test_new_file_over_limit_is_violation(self):
        self.repo.write("seed.txt", "s\n")
        self.repo.stage("seed.txt")
        self.repo.commit("seed")
        self.repo.write("new.rs", _lines(LIMIT + 1))
        self.repo.stage("new.rs")
        r = self.repo.run()
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        self.assertIn("new.rs", r.stdout + r.stderr)
        self.assertIn(str(LIMIT + 1), r.stdout + r.stderr)

    def test_baselined_file_over_cap_is_violation(self):
        self._head_with_baseline("2500 big.rs\n", big_lines=2500)
        self.repo.write("big.rs", _lines(2501))
        self.repo.stage("big.rs")
        r = self.repo.run()
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        out = r.stdout + r.stderr
        self.assertIn("big.rs", out)
        self.assertIn("2501", out)
        self.assertIn("2500", out)

    def test_staged_baseline_raising_cap_is_violation(self):
        self._head_with_baseline("2500 big.rs\n", big_lines=2500)
        self.repo.write(BASELINE, "2600 big.rs\n")
        self.repo.stage(BASELINE)
        r = self.repo.run()
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        self.assertIn("big.rs", r.stdout + r.stderr)

    def test_staged_baseline_new_entry_over_limit_is_violation(self):
        self._head_with_baseline("2500 big.rs\n", big_lines=2500)
        self.repo.write(BASELINE, "2500 big.rs\n2100 other.rs\n")
        self.repo.stage(BASELINE)
        r = self.repo.run()
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        self.assertIn("other.rs", r.stdout + r.stderr)

    def test_unparsable_baseline_line_is_undetermined(self):
        self.repo.write(BASELINE, "not-a-number big.rs\n")
        self.repo.write("a.rs", _lines(3))
        self.repo.stage(BASELINE, "a.rs")
        r = self.repo.run()
        self.assertEqual(r.returncode, 2, r.stdout + r.stderr)
        self.assertIn("not-a-number", r.stdout + r.stderr)

    def test_precommit_hook_invokes_the_check(self):
        text = (REPO_ROOT / ".githooks" / "pre-commit").read_text()
        self.assertIn("check-file-growth.py", text)

    # ---- CONTROL-ish (must pass after a correct implementation) ----

    def test_file_at_limit_passes(self):
        self.repo.write("seed.txt", "s\n")
        self.repo.stage("seed.txt")
        self.repo.commit("seed")
        self.repo.write("ok.rs", _lines(LIMIT))
        self.repo.stage("ok.rs")
        r = self.repo.run()
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)

    def test_final_line_without_newline_counts(self):
        self.repo.write("seed.txt", "s\n")
        self.repo.stage("seed.txt")
        self.repo.commit("seed")
        self.repo.write("nonl.py", _lines(LIMIT) + "last")  # 2001 lines
        self.repo.stage("nonl.py")
        r = self.repo.run()
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)

    def test_baselined_file_shrinking_passes(self):
        self._head_with_baseline("2500 big.rs\n", big_lines=2500)
        self.repo.write("big.rs", _lines(2400))
        self.repo.stage("big.rs")
        r = self.repo.run()
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)

    def test_lowering_a_cap_passes(self):
        self._head_with_baseline("2500 big.rs\n", big_lines=2500)
        self.repo.write(BASELINE, "2400 big.rs\n")
        self.repo.stage(BASELINE)
        r = self.repo.run()
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)

    def test_deleting_a_baseline_entry_passes(self):
        self._head_with_baseline("2500 big.rs\n", big_lines=100)
        self.repo.write(BASELINE, "# empty\n\n")
        self.repo.stage(BASELINE)
        r = self.repo.run()
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)

    def test_unstaged_working_tree_growth_is_ignored(self):
        self.repo.write("big.rs", _lines(100))
        self.repo.stage("big.rs")
        self.repo.commit("base")
        self.repo.write("big.rs", _lines(3000))  # NOT staged
        self.repo.write("small.rs", _lines(2))
        self.repo.stage("small.rs")
        r = self.repo.run()
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)

    def test_non_source_extension_is_ignored(self):
        self.repo.write("seed.txt", "s\n")
        self.repo.stage("seed.txt")
        self.repo.commit("seed")
        self.repo.write("notes.md", _lines(5000))
        self.repo.stage("notes.md")
        r = self.repo.run()
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)


if __name__ == "__main__":
    unittest.main(verbosity=1)
