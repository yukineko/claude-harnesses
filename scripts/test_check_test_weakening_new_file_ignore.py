#!/usr/bin/env python3
"""RED repro: check-test-weakening blocks a NEWLY ADDED Rust test file whose
`#[ignore = "backlog <id>: ..."]` tests are intentional RED repros.

Observed 2026-11-05 at main e97692da:

    [BLOCK] crates/compass/tests/backlog_scout_20261105.rs: ignore-added (#[ignore] attributes 0 -> 3)

The file did not exist at the base, so no existing test was weakened. The
backlog filing protocol (crates/backlog/README.ja.md) requires committing such
repros. The Python half of this same scanner already exempts new skipped
repros ("a new skipped test hides no coverage that existed"); the Rust half
counts `#[ignore]` per FILE, so 0 -> N on a new file fires.

Contract asserted: a brand-new file whose ignores carry a `backlog` reason
must not be reported as `ignore-added`. A control asserts that adding an
ignore to an EXISTING test is still blocked (passes today).

Repro:  python3 -m pytest scripts/test_check_test_weakening_new_file_ignore.py::NewFileIgnore::test_new_file_with_backlog_ignores_is_not_blocked
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
#[test]
fn one() {
    assert_eq!(1, 1);
}
"""

RS_NEW_REPRO = """\
#[test]
#[ignore = "backlog deadbeef: intentional RED repro"]
fn repro_a() {
    assert_eq!(1, 2);
}

#[test]
#[ignore = "backlog deadbeef: intentional RED repro"]
fn repro_b() {
    assert_eq!(1, 2);
}
"""


class NewFileIgnore(unittest.TestCase):
    def setUp(self) -> None:
        td = tempfile.TemporaryDirectory(prefix="twnewfile.")
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
        (self.repo / "crates" / "c" / "tests").mkdir(parents=True)
        self.git("init", "-q", "-b", "main")
        self.existing = self.repo / "crates" / "c" / "tests" / "existing.rs"
        self.existing.write_text(RS_BASE)
        self.git("add", "-A")
        self.git("commit", "-q", "-m", "C0")
        self.git("checkout", "-q", "-b", "feat")

    def git(self, *args: str) -> str:
        return subprocess.run(["git", *args], cwd=self.repo, env=self.env, check=True,
                              capture_output=True, text=True).stdout

    def scan(self) -> tuple[int, str]:
        msg = self.tmp / "COMMIT_EDITMSG"
        msg.write_text("add repros\n")
        r = subprocess.run([sys.executable, str(SCRIPT), "--repo", str(self.repo),
                            "--pending-msg", str(msg)],
                           cwd=self.repo, env=self.env, capture_output=True,
                           text=True, timeout=60)
        return r.returncode, r.stdout + r.stderr

    def test_control_ignore_on_existing_test_still_blocks(self) -> None:
        self.existing.write_text(RS_BASE.replace("#[test]\n", "#[test]\n#[ignore]\n"))
        self.git("add", "-A")
        rc, out = self.scan()
        self.assertEqual(rc, 1, out)
        self.assertIn("ignore-added", out)

    def test_new_file_with_backlog_ignores_is_not_blocked(self) -> None:
        new = self.repo / "crates" / "c" / "tests" / "backlog_repro.rs"
        new.write_text(RS_NEW_REPRO)
        self.git("add", "-A")
        self.assertIn("backlog_repro.rs", self.git("diff", "--cached", "--name-only"),
                      "anti-vacuity: the new file is staged")
        rc, out = self.scan()
        self.assertNotIn("ignore-added", out,
                         "a file that did not exist at the base cannot have had "
                         f"coverage weakened; rc={rc} output={out!r}")
        self.assertEqual(rc, 0, out)


if __name__ == "__main__":
    unittest.main()
