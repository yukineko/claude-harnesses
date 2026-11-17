#!/usr/bin/env python3
"""RED tests: check-version-bumped.py must treat crates/riskgrade as a SHARED crate.

User ruling 2026-11-17: crates/riskgrade/ (a pure library crate with no
.claude-plugin/plugin.json, linked into the condukt plugin) gets the same
bump-on-change rule as crates/harness-core/:

  * a LINKED change under crates/riskgrade/ (anything except tests/ and *.md)
    with [package].version in crates/riskgrade/Cargo.toml not moving up must be
    BLOCKED (non-zero exit) and the message must name riskgrade;
  * changes confined to crates/riskgrade/tests/ or *.md need no bump;
  * a riskgrade crate absent at base is not a "change".

Stdlib-only. Every case runs the REAL scripts/check-version-bumped.py against a
throwaway git repository (working-tree mode, --base HEAD, the pre-commit mode),
plus one --head (post-merge) mode case. Nothing touches the real repository.

    python3 scripts/test_check_version_bumped_riskgrade.py

Written by an independent test author (CLAUDE.md §2(a)), not the implementer.
The harness-core control pins that the fixture is wired the way the real gate
sees a shared crate, so a RED on the riskgrade case cannot be a fixture artefact.
"""
from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

_SCRIPTS = Path(__file__).resolve().parent
_GATE = _SCRIPTS / "check-version-bumped.py"
_ENV = dict(
    PATH=os.environ.get("PATH", "/usr/bin:/bin"),
    LC_ALL="C",
    GIT_CONFIG_NOSYSTEM="1",
    GIT_CONFIG_GLOBAL=os.devnull,
    GIT_AUTHOR_NAME="t", GIT_AUTHOR_EMAIL="t@example.invalid",
    GIT_COMMITTER_NAME="t", GIT_COMMITTER_EMAIL="t@example.invalid",
)


def _git(cwd, *args, check=True):
    p = subprocess.run(["git", *args], cwd=str(cwd), env=_ENV,
                       capture_output=True, text=True)
    if check and p.returncode != 0:
        raise AssertionError("git %s failed: %s" % (args, p.stderr))
    return p


def _toml(name, version):
    return '[package]\nname = "%s"\nversion = "%s"\nedition = "2021"\n' % (name, version)


class Repo:
    def __init__(self):
        self.root = Path(tempfile.mkdtemp(prefix="vbump-riskgrade-")).resolve()
        _git(self.root, "init", "-q", "-b", "main")
        _git(self.root, "config", "commit.gpgsign", "false")

    def cleanup(self):
        shutil.rmtree(self.root, ignore_errors=True)

    def write(self, rel, text):
        p = self.root / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(text)

    def commit_all(self, msg="c"):
        _git(self.root, "add", "-A")
        _git(self.root, "commit", "-q", "-m", msg)

    def gate(self, *args):
        return subprocess.run([sys.executable, str(_GATE), *args],
                              cwd=str(self.root), env=_ENV,
                              capture_output=True, text=True)


def _base_layout(repo, with_riskgrade=True):
    """Minimal layout of the real repo: harness-core shared crate, one plugin
    (condukt, which links riskgrade), and optionally the riskgrade library."""
    repo.write("crates/harness-core/Cargo.toml", _toml("harness-core", "0.1.0"))
    repo.write("crates/harness-core/src/lib.rs", "// core v1\n")
    repo.write("crates/condukt/.claude-plugin/plugin.json",
               json.dumps({"name": "condukt", "version": "1.0.0"}))
    repo.write("crates/condukt/Cargo.toml", _toml("condukt", "1.0.0"))
    repo.write("crates/condukt/src/main.rs", "fn main() {}\n")
    if with_riskgrade:
        repo.write("crates/riskgrade/Cargo.toml", _toml("riskgrade", "0.1.0"))
        repo.write("crates/riskgrade/src/lib.rs", "// riskgrade v1\n")


def _show(p):
    return "rc=%d\n--- stdout ---\n%s--- stderr ---\n%s" % (p.returncode, p.stdout, p.stderr)


class Base(unittest.TestCase):
    def repo(self, with_riskgrade=True):
        r = Repo()
        self.addCleanup(r.cleanup)
        _base_layout(r, with_riskgrade=with_riskgrade)
        r.commit_all("base")
        return r


class RiskgradeIsSharedCrate(Base):
    # ---- the RED cases -------------------------------------------------------

    def test_riskgrade_linked_change_without_bump_is_blocked(self):
        r = self.repo()
        r.write("crates/riskgrade/src/lib.rs", "// riskgrade v2 (no bump)\n")
        p = r.gate("--base", "HEAD")
        self.assertNotEqual(p.returncode, 0,
                            "unbumped linked riskgrade change was allowed:\n" + _show(p))
        self.assertIn("riskgrade", p.stderr,
                      "block message does not name riskgrade:\n" + _show(p))

    def test_riskgrade_linked_change_without_bump_is_blocked_head_mode(self):
        r = self.repo()
        r.write("crates/riskgrade/src/lib.rs", "// riskgrade v2 (no bump)\n")
        r.commit_all("unbumped riskgrade edit")
        p = r.gate("--base", "HEAD~1", "--head", "HEAD")
        self.assertNotEqual(p.returncode, 0,
                            "unbumped riskgrade change allowed in --head mode:\n" + _show(p))
        self.assertIn("riskgrade", p.stderr, _show(p))

    def test_riskgrade_manifest_nonversion_edit_without_bump_is_blocked(self):
        # Cargo.toml itself is linked (not tests/, not *.md): editing it without
        # moving the version is still a linked change.
        r = self.repo()
        r.write("crates/riskgrade/Cargo.toml",
                _toml("riskgrade", "0.1.0") + "\n[dependencies]\n")
        p = r.gate("--base", "HEAD")
        self.assertNotEqual(p.returncode, 0, _show(p))
        self.assertIn("riskgrade", p.stderr, _show(p))

    # ---- controls ------------------------------------------------------------

    def test_control_a_riskgrade_change_with_bump_passes(self):
        r = self.repo()
        r.write("crates/riskgrade/src/lib.rs", "// riskgrade v2\n")
        r.write("crates/riskgrade/Cargo.toml", _toml("riskgrade", "0.1.1"))
        p = r.gate("--base", "HEAD")
        self.assertEqual(p.returncode, 0, _show(p))

    def test_control_b1_riskgrade_tests_only_change_passes_without_bump(self):
        r = self.repo()
        r.write("crates/riskgrade/tests/it.rs", "#[test] fn t() {}\n")
        p = r.gate("--base", "HEAD")
        self.assertEqual(p.returncode, 0, _show(p))

    def test_control_b2_riskgrade_md_only_change_passes_without_bump(self):
        r = self.repo()
        r.write("crates/riskgrade/README.md", "# riskgrade\n")
        p = r.gate("--base", "HEAD")
        self.assertEqual(p.returncode, 0, _show(p))

    def test_control_new_riskgrade_absent_at_base_passes(self):
        r = self.repo(with_riskgrade=False)
        r.write("crates/riskgrade/Cargo.toml", _toml("riskgrade", "0.1.0"))
        r.write("crates/riskgrade/src/lib.rs", "// new\n")
        p = r.gate("--base", "HEAD")
        self.assertEqual(p.returncode, 0, _show(p))

    def test_control_c_harness_core_unbumped_still_blocked(self):
        r = self.repo()
        r.write("crates/harness-core/src/lib.rs", "// core v2 (no bump)\n")
        p = r.gate("--base", "HEAD")
        self.assertEqual(p.returncode, 1, _show(p))
        self.assertIn("harness-core", p.stderr, _show(p))

    def test_control_c2_harness_core_bumped_passes(self):
        r = self.repo()
        r.write("crates/harness-core/src/lib.rs", "// core v2\n")
        r.write("crates/harness-core/Cargo.toml", _toml("harness-core", "0.1.1"))
        p = r.gate("--base", "HEAD")
        self.assertEqual(p.returncode, 0, _show(p))

    def test_control_both_shared_crates_unbumped_names_both(self):
        # Both shared crates changed without a bump: neither may mask the other.
        r = self.repo()
        r.write("crates/harness-core/src/lib.rs", "// core v2 (no bump)\n")
        r.write("crates/riskgrade/src/lib.rs", "// riskgrade v2 (no bump)\n")
        p = r.gate("--base", "HEAD")
        self.assertNotEqual(p.returncode, 0, _show(p))
        self.assertIn("harness-core", p.stderr, _show(p))
        self.assertIn("riskgrade", p.stderr, _show(p))


if __name__ == "__main__":
    unittest.main(verbosity=2)
