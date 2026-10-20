#!/usr/bin/env python3
"""Independent tests (backlog 3a8e3b73) for check-gate-protection.py rules the
implementer added beyond the coordinator ruling. Each rule has a firing test
with the pinned exit code and a control on the nearest legal variant.
Reuses the fixture helpers of test_check_gate_protection.py.
"""
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from test_check_gate_protection import BASELINE, SCRIPT, decl, git, make_repo, run  # noqa: E402


class ExtraRules(unittest.TestCase):
    def setUp(self):
        self._t = tempfile.TemporaryDirectory()
        self.addCleanup(self._t.cleanup)
        self.root = Path(self._t.name) / "repo"
        self.root.mkdir()

    def rc(self):
        r = run(self.root)
        return r.returncode, f"stdout={r.stdout!r} stderr={r.stderr!r}"

    def assertRc(self, want):
        got, msg = self.rc()
        self.assertEqual(got, want, msg)

    # (a) declared AND still PENDING (baseline committed at HEAD, so no growth)
    def test_a_declared_but_still_pending_fails(self):
        make_repo(self.root, baseline="beta\n")  # both declared
        self.assertRc(1)

    def test_a_control_declared_and_removed_from_baseline_passes(self):
        make_repo(self.root, baseline="")
        self.assertRc(0)

    # (b) baseline entry not in BLOCKING_GATES (present at HEAD, so no growth)
    def test_b_baseline_entry_not_blocking_gate_fails(self):
        make_repo(self.root, baseline="gamma\n")
        self.assertRc(1)

    def test_b_control_baseline_entry_is_blocking_gate_passes(self):
        make_repo(self.root, decls={"alpha": decl(), "beta": None}, baseline="beta\n")
        self.assertRc(0)

    # (c) baseline absent at HEAD: treated as empty, every entry is growth
    def _untrack_baseline(self):
        git(self.root, "rm", "-q", "--cached", BASELINE)
        git(self.root, "commit", "-qm", "drop baseline from HEAD")
        self.assertTrue((self.root / BASELINE).is_file())

    def test_c_baseline_absent_at_head_makes_entries_growth(self):
        make_repo(self.root, decls={"alpha": decl(), "beta": None}, baseline="beta\n")
        self._untrack_baseline()
        self.assertRc(1)

    def test_c_control_baseline_absent_at_head_but_empty_passes(self):
        make_repo(self.root, baseline="")
        self._untrack_baseline()
        self.assertRc(0)

    # (d) more than one Protection literal for a gate
    def test_d_two_protection_literals_is_undetermined(self):
        make_repo(self.root)
        second = self.root / "crates/alpha/src/other.rs"
        second.write_text(decl().replace("P:", "Q:"))
        self.assertRc(2)

    def test_d_control_one_literal_per_gate_passes(self):
        make_repo(self.root)
        (self.root / "crates/alpha/src/other.rs").write_text("pub fn f() {}\n")
        self.assertRc(0)

    # (e) git cannot report HEAD's baseline
    def _run_no_ancestor_repo(self):
        env = dict(os.environ, GIT_CEILING_DIRECTORIES=str(self.root.parent))
        return subprocess.run(
            ["python3", str(SCRIPT), "--repo", str(self.root)],
            capture_output=True, text=True, env=env,
        )

    def test_e_git_failure_is_undetermined(self):
        make_repo(self.root)
        shutil.rmtree(self.root / ".git")
        r = self._run_no_ancestor_repo()
        self.assertEqual(r.returncode, 2, f"stdout={r.stdout!r} stderr={r.stderr!r}")

    def test_e_control_with_git_passes(self):
        make_repo(self.root)
        r = self._run_no_ancestor_repo()
        self.assertEqual(r.returncode, 0, f"stdout={r.stdout!r} stderr={r.stderr!r}")


if __name__ == "__main__":
    unittest.main()
