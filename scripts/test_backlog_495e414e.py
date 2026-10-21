#!/usr/bin/env python3
"""backlog 495e414e: lint-changed-crates.sh reports a Cargo.lock-only (or root
Cargo.toml-only) change as "no crate touched — nothing to lint" and exits 0.

A lockfile / workspace-manifest change can change how EVERY crate builds and
lints, so "nothing to lint" + exit 0 reads as "checked, clean" for a change
whose lint impact was never examined — the silence-read-as-checked class the
script's own header (d470a478) says it must not produce.

The property tested: for such a change the script must NOT combine exit 0 with
the "nothing to lint" claim (it may lint, fail closed, or disclose the
unexamined scope — any of those satisfies this test). A control asserts that a
docs-only change still yields "nothing to lint" + exit 0, so the test does not
just demand that every run be non-zero.

RED repro: python3 -m pytest --runxfail scripts/test_backlog_495e414e.py
"""
from __future__ import annotations

import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
SCRIPT = HERE / "lint-changed-crates.sh"

GIT_ENV = {
    "GIT_AUTHOR_NAME": "t", "GIT_AUTHOR_EMAIL": "t@example.invalid",
    "GIT_COMMITTER_NAME": "t", "GIT_COMMITTER_EMAIL": "t@example.invalid",
    "GIT_CONFIG_NOSYSTEM": "1",
}


class LockfileOnlyChange(unittest.TestCase):
    def setUp(self) -> None:
        self.repo = Path(os.path.realpath(tempfile.mkdtemp(prefix="bl495e414e-")))
        self.addCleanup(shutil.rmtree, self.repo, True)
        self.env = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}
        self.env.update(GIT_ENV)
        (self.repo / "scripts").mkdir()
        shutil.copy2(HERE / "cap-target-dir.sh", self.repo / "scripts" / "cap-target-dir.sh")
        (self.repo / "Cargo.toml").write_text(
            '[workspace]\nresolver = "2"\nmembers = ["crates/a"]\n'
        )
        (self.repo / "crates" / "a" / "src").mkdir(parents=True)
        (self.repo / "crates" / "a" / "Cargo.toml").write_text(
            '[package]\nname = "a"\nversion = "0.0.0"\nedition = "2021"\n'
        )
        (self.repo / "crates" / "a" / "src" / "lib.rs").write_text("pub fn f() {}\n")
        (self.repo / "Cargo.lock").write_text("version = 3\n")
        (self.repo / "README.md").write_text("r\n")
        self.git("init", "-q", "-b", "main")
        self.git("-c", "core.hooksPath=/dev/null", "add", "-A")
        self.git("-c", "core.hooksPath=/dev/null", "commit", "-q", "-m", "i")

    def git(self, *args: str) -> None:
        subprocess.run(["git", *args], cwd=self.repo, env=self.env, check=True,
                       capture_output=True)

    def run_script(self) -> subprocess.CompletedProcess:
        return subprocess.run(["bash", str(SCRIPT)], cwd=self.repo, env=self.env,
                              capture_output=True, text=True, timeout=600)

    def assert_not_claimed_clean(self, what: str) -> None:
        proc = self.run_script()
        claimed = proc.returncode == 0 and "nothing to lint" in proc.stdout
        self.assertFalse(
            claimed,
            "%s-only change reported as nothing-to-lint with exit 0:\nrc=%d\n%s%s"
            % (what, proc.returncode, proc.stdout, proc.stderr),
        )

    def test_control_docs_only_change_is_nothing_to_lint(self) -> None:
        (self.repo / "README.md").write_text("changed\n")
        proc = self.run_script()
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)
        self.assertIn("nothing to lint", proc.stdout)

    def test_cargo_lock_only_change_is_not_nothing_to_lint(self) -> None:
        (self.repo / "Cargo.lock").write_text("version = 4\n")
        self.assert_not_claimed_clean("Cargo.lock")

    def test_root_manifest_only_change_is_not_nothing_to_lint(self) -> None:
        with (self.repo / "Cargo.toml").open("a") as f:
            f.write('\n[workspace.lints.clippy]\nunwrap_used = "deny"\n')
        self.assert_not_claimed_clean("root Cargo.toml")


if __name__ == "__main__":
    unittest.main()
