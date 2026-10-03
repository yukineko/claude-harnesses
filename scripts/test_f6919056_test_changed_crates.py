"""Regression test for backlog f6919056 item 1: test-changed-crates.sh.

A touched crate dir whose Cargo.toml is unreadable, or has no extractable
[package] name, used to be dropped silently; if it was the only touched crate
the script printed "nothing to test" and exited 0 ("could not determine" read as
"nothing to test"). Undetermined must be non-zero (CLAUDE.md section 3).

Run: python3 -m pytest scripts/test_f6919056_test_changed_crates.py
"""
import os
import shutil
import stat
import subprocess
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent / "test-changed-crates.sh"


class TestChangedCratesUndetermined(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="f6919056-"))
        self.addCleanup(shutil.rmtree, self.tmp, True)
        self.repo = self.tmp / "repo"
        (self.repo / "scripts").mkdir(parents=True)
        # Stub for the collaborator the script calls on the success path.
        cap = self.repo / "scripts" / "cap-target-dir.sh"
        cap.write_text("#!/usr/bin/env bash\nexit 0\n")
        cap.chmod(0o755)
        # Fake cargo that records its calls; real cargo must never run.
        self.bin = self.tmp / "bin"
        self.bin.mkdir()
        self.calls = self.tmp / "cargo-calls.txt"
        cargo = self.bin / "cargo"
        cargo.write_text('#!/usr/bin/env bash\necho "$@" >> "%s"\nexit 0\n' % self.calls)
        cargo.chmod(0o755)
        self.env = dict(os.environ, PATH="%s:%s" % (self.bin, os.environ["PATH"]))
        self.env.pop("GIT_DIR", None)
        self.env.pop("GIT_WORK_TREE", None)
        self.git("init", "-q")
        self.git("-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q",
                 "--allow-empty", "-m", "base")

    def git(self, *args):
        subprocess.run(["git", *args], cwd=self.repo, env=self.env, check=True,
                       capture_output=True)

    def write_manifest(self, crate, text):
        d = self.repo / "crates" / crate
        d.mkdir(parents=True, exist_ok=True)
        m = d / "Cargo.toml"
        m.write_text(text)
        return m

    def run_script(self):
        return subprocess.run(["bash", str(SCRIPT)], cwd=self.repo, env=self.env,
                              capture_output=True, text=True, timeout=60)

    def cargo_calls(self):
        return self.calls.read_text() if self.calls.exists() else ""

    def test_control_valid_manifest_is_processed(self):
        self.write_manifest("good", '[package]\nname = "good-pkg"\nversion = "0.1.0"\n')
        r = self.run_script()
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertIn("test -p good-pkg", self.cargo_calls())

    def test_manifest_without_package_name_is_not_clean(self):
        self.write_manifest("noname", '[workspace]\nmembers = []\n')
        r = self.run_script()
        self.assertNotEqual(r.returncode, 0,
                            "undetermined crate read as clean:\n" + r.stdout + r.stderr)
        self.assertNotIn("nothing to test", r.stdout)
        self.assertEqual(self.cargo_calls(), "")

    @unittest.skipIf(hasattr(os, "geteuid") and os.geteuid() == 0,
                     "chmod 000 does not make a file unreadable for root")
    def test_unreadable_manifest_is_not_clean(self):
        m = self.write_manifest("locked", '[package]\nname = "locked-pkg"\n')
        m.chmod(0)
        self.addCleanup(m.chmod, stat.S_IRUSR | stat.S_IWUSR)
        self.assertFalse(os.access(m, os.R_OK))
        r = self.run_script()
        self.assertNotEqual(r.returncode, 0,
                            "unreadable manifest read as clean:\n" + r.stdout + r.stderr)
        self.assertNotIn("nothing to test", r.stdout)
        self.assertEqual(self.cargo_calls(), "")


if __name__ == "__main__":
    unittest.main()
