#!/usr/bin/env python3
"""RED repro for backlog d470a478.

`scripts/lint-changed-crates.sh` (donegate's fmt/clippy check) lints ONLY the
crates whose files changed. When a shared crate changes (harness-core above all)
its dependents' fmt/clippy state is never checked by anyone — GitHub Actions is
banned (CLAUDE.md §7), so the CI backstop the script's canon comment assumes
does not exist — and the success line `lint-changed-crates: all green` does not
say that only the touched crates were checked.

Throwaway cargo workspace: `core` and `app` (app depends on core). Only core is
modified. A stub `cargo` on PATH records every invocation. The property: either
`app` is linted too (candidate a), or the green line states its scope
(candidate c). Fixed by candidate c: the green line names its scope.

    python3 scripts/test_backlog_d470a478.py
"""

from __future__ import annotations

import os
import shutil
import stat
import subprocess
import tempfile
import unittest
from pathlib import Path

_HERE = Path(__file__).resolve().parent
_SCRIPT = _HERE / "lint-changed-crates.sh"
_CAP = _HERE / "cap-target-dir.sh"

STUB_CARGO = """#!/bin/sh
echo "$*" >> "{log}"
exit 0
"""


def _git(cwd, *args):
    subprocess.run(["git", *args], cwd=cwd, check=True, capture_output=True)


class DependentsOfAChangedCrateAreCovered(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="d470a478."))
        self.addCleanup(shutil.rmtree, self.tmp, True)
        repo = self.tmp / "repo"
        (repo / "scripts").mkdir(parents=True)
        shutil.copy(_CAP, repo / "scripts" / "cap-target-dir.sh")
        (repo / "Cargo.toml").write_text(
            '[workspace]\nresolver = "2"\nmembers = ["crates/core", "crates/app"]\n')
        for name, deps in (("core", ""), ("app", 'core = { path = "../core" }\n')):
            d = repo / "crates" / name
            (d / "src").mkdir(parents=True)
            (d / "Cargo.toml").write_text(
                '[package]\nname = "%s"\nversion = "0.0.0"\nedition = "2021"\n\n'
                "[dependencies]\n%s" % (name, deps))
            (d / "src" / "lib.rs").write_text("pub fn f() {}\n")
        _git(repo, "init", "-q")
        _git(repo, "config", "user.email", "t@t")
        _git(repo, "config", "user.name", "t")
        _git(repo, "add", "-A")
        _git(repo, "commit", "-qm", "init")
        # Only the shared crate changes.
        (repo / "crates/core/src/lib.rs").write_text("pub fn f() { let _x = 1; }\n")
        self.repo = repo
        bindir = self.tmp / "bin"
        bindir.mkdir()
        self.log = self.tmp / "cargo.log"
        stub = bindir / "cargo"
        stub.write_text(STUB_CARGO.format(log=self.log))
        stub.chmod(stub.stat().st_mode | stat.S_IEXEC)
        self.env = dict(os.environ)
        self.env["PATH"] = str(bindir) + os.pathsep + "/usr/bin:/bin"

    def test_dependent_is_linted_or_green_line_states_scope(self):
        p = subprocess.run(["bash", str(_SCRIPT)], cwd=self.repo, env=self.env,
                           capture_output=True, text=True)
        self.assertEqual(p.returncode, 0, p.stdout + p.stderr)
        calls = self.log.read_text() if self.log.exists() else ""
        # Anti-vacuity: the touched crate really was linted.
        self.assertIn("clippy -p core", calls)
        dependent_linted = "clippy -p app" in calls
        # The script's own name contains "changed", so look past the prefix.
        green = [l.replace("lint-changed-crates:", "")
                 for l in p.stdout.splitlines() if "all green" in l]
        scope_stated = any(
            ("touched" in l or "changed" in l or "only" in l) for l in green)
        self.assertTrue(
            dependent_linted or scope_stated,
            "core changed, its dependent app was not linted, and the green line "
            "does not state the scope: calls=%r stdout=%r" % (calls, p.stdout),
        )


if __name__ == "__main__":
    unittest.main()
