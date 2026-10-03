#!/usr/bin/env python3
"""Audit reproduction tests for backlog shard s05-misc-a.

Every test named after a backlog id asserts the property the ticket says is
BROKEN. They were each observed RED on HEAD e70d48dd and are skipped by default
so the suite stays green. Run them (expecting failures while the defect is open)
with:

    AUDIT_RUN_OPEN=1 python3 -m unittest scripts.test_backlog_s05_repros

Remove the skip for an id when its defect is fixed.

Tests without the `open_defect` decorator are controls that must pass today.
"""
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent
REPO = SCRIPTS.parent


def open_defect(backlog_id):
    return unittest.skipUnless(
        os.environ.get("AUDIT_RUN_OPEN"),
        f"backlog {backlog_id}: open defect, remove skip when fixed",
    )


def _env(**extra):
    env = dict(os.environ)
    env.update(
        GIT_CONFIG_GLOBAL=os.devnull,
        GIT_CONFIG_NOSYSTEM="1",
        GIT_AUTHOR_NAME="t",
        GIT_AUTHOR_EMAIL="t@example.invalid",
        GIT_COMMITTER_NAME="t",
        GIT_COMMITTER_EMAIL="t@example.invalid",
    )
    env.update(extra)
    return env


def _git(cwd, *args):
    r = subprocess.run(["git", *args], cwd=cwd, env=_env(), capture_output=True, text=True)
    if r.returncode != 0:
        raise AssertionError(f"git {args} failed: {r.stderr}")
    return r.stdout


class EditGuardDuringMerge(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="s05m-")).resolve()
        self.m = self.tmp / "main"
        self.m.mkdir()
        _git(self.m, "init", "-q", "-b", "main")
        (self.m / "f.txt").write_text("base\n")
        _git(self.m, "add", "f.txt")
        _git(self.m, "commit", "-q", "-m", "base")
        _git(self.m, "checkout", "-q", "-b", "other")
        (self.m / "f.txt").write_text("other\n")
        _git(self.m, "commit", "-q", "-am", "other")
        _git(self.m, "checkout", "-q", "main")
        (self.m / "f.txt").write_text("mine\n")
        _git(self.m, "commit", "-q", "-am", "mine")
        subprocess.run(["git", "merge", "other"], cwd=self.m, env=_env(), capture_output=True)
        assert (self.m / ".git" / "MERGE_HEAD").exists(), "merge did not conflict"

    def tearDown(self):
        shutil.rmtree(self.tmp, ignore_errors=True)

    def _edit(self, path):
        payload = {"tool_name": "Edit", "tool_input": {"file_path": str(path), "old_string": "a", "new_string": "b"}}
        return subprocess.run(
            [sys.executable, str(SCRIPTS / "guard-maintree-edit.py")],
            input=json.dumps(payload), capture_output=True, text=True,
            cwd=str(self.m), env=_env(CLAUDE_PROJECT_DIR=str(self.m)),
        ).returncode

    def test_control_unrelated_file_stays_refused_during_merge(self):
        self.assertEqual(self._edit(self.m / "other.txt"), 2)

    @open_defect("ac2ba31d")
    def test_ac2ba31d_unmerged_path_edit_allowed_during_merge(self):
        self.assertEqual(self._edit(self.m / "f.txt"), 0)


class PluginCacheResidue(unittest.TestCase):
    """backlog 9b64f427 — plugin_cache.scan residue: undetermined read as clean/removable."""

    @classmethod
    def setUpClass(cls):
        sys.path.insert(0, str(SCRIPTS))
        import plugin_cache
        cls.pc = plugin_cache

    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="s05c-")).resolve()

    def tearDown(self):
        shutil.rmtree(self.tmp, ignore_errors=True)

    def test_9b64f427_1_dangling_cache_root_is_a_problem(self):
        root = self.tmp / "cache"
        os.symlink(self.tmp / "nowhere", root)
        stale, problems = self.pc.scan(str(root), {"alpha": "1.0.0"})
        self.assertTrue(problems, "dangling cache root resolved to clean")

    def test_9b64f427_2_unrecognised_in_use_marker_holds_the_dir(self):
        d = self.tmp / "c" / "alpha" / "0.9.0" / ".in_use"
        d.mkdir(parents=True)
        (d / "pid-12345").write_text("")
        stale, problems = self.pc.scan(str(self.tmp / "c"), {"alpha": "1.0.0"})
        self.assertFalse(any(s.removable for s in stale) and not problems,
                         "unrecognised holder record read as 'nobody holds it'")

    def test_9b64f427_3_unreadable_crate_is_a_problem(self):
        pj = self.tmp / "crates" / "alpha" / ".claude-plugin" / "plugin.json"
        pj.parent.mkdir(parents=True)
        os.symlink(self.tmp / "gone.json", pj)
        versions, problems = self.pc.source_versions(str(self.tmp / "crates"))
        self.assertTrue(problems or "alpha" in versions, "unreadable crate silently dropped")

    def test_9b64f427_4_non_version_directory_is_not_removable(self):
        (self.tmp / "c" / "alpha" / "node_modules").mkdir(parents=True)
        stale, problems = self.pc.scan(str(self.tmp / "c"), {"alpha": "1.0.0"})
        self.assertFalse(any(s.removable for s in stale), "non-version dir handed to rmtree")


class RolloutProvenance(unittest.TestCase):
    def test_649d15f6_recopy_keeps_deployed_from_json(self):
        tmp = Path(tempfile.mkdtemp(prefix="s05r-")).resolve()
        try:
            (tmp / "src" / "bin").mkdir(parents=True)
            (tmp / "dst" / "bin").mkdir(parents=True)
            (tmp / "src" / "bin" / "foo").write_text("launcher\n")
            (tmp / "dst" / "bin" / "foo-darwin-arm64").write_text("bin\n")
            (tmp / "dst" / ".deployed-from.json").write_text("{}\n")
            fn = subprocess.run(
                ["sed", "-n", "/^copy_plugin_dir()/,/^}/p", str(SCRIPTS / "rollout-plugins.sh")],
                capture_output=True, text=True).stdout
            self.assertTrue(fn.strip(), "could not extract copy_plugin_dir")
            (tmp / "fn.bash").write_text(fn)
            subprocess.run(["bash", "-c", f"source {tmp}/fn.bash; copy_plugin_dir {tmp}/src {tmp}/dst"],
                           check=True, capture_output=True)
            self.assertTrue((tmp / "dst" / ".deployed-from.json").exists(),
                            "recopy deleted the provenance manifest")
        finally:
            shutil.rmtree(tmp, ignore_errors=True)


class ScriptGates(unittest.TestCase):
    def test_f6919056_check_versions_without_crates_dir_is_not_ok(self):
        tmp = Path(tempfile.mkdtemp(prefix="s05v-")).resolve()
        try:
            (tmp / "scripts").mkdir()
            shutil.copy2(SCRIPTS / "check-versions.sh", tmp / "scripts" / "check-versions.sh")
            r = subprocess.run(["bash", str(tmp / "scripts" / "check-versions.sh")], capture_output=True, text=True)
            self.assertNotEqual(r.returncode, 0, "parity 'OK' with zero crates inspected: " + r.stdout)
        finally:
            shutil.rmtree(tmp, ignore_errors=True)

    def test_f6919056_bench_gate_with_floorless_threshold_still_flags_a_regression(self):
        tmp = Path(tempfile.mkdtemp(prefix="s05b-")).resolve()
        try:
            d = tmp / "d.jsonl"
            d.write_text('{"resolution_rate":0.9}\n{"resolution_rate":0.1}\n')
            r = subprocess.run([sys.executable, str(SCRIPTS / "check-bench-regression.py"),
                                "--dashboard", str(d), "--threshold", "1.0"], capture_output=True, text=True)
            self.assertNotEqual(r.returncode, 0, "threshold 1.0 disabled the gate: " + r.stdout)
        finally:
            shutil.rmtree(tmp, ignore_errors=True)

    @open_defect("4883ad97")
    def test_4883ad97_clean_tree_does_not_claim_plugins_were_scanned(self):
        r = subprocess.run([sys.executable, str(SCRIPTS / "check-version-bumped.py"), "--base", "HEAD"],
                           cwd=str(REPO), capture_output=True, text=True)
        out = r.stdout + r.stderr
        if "no changed plugin" not in out:
            self.skipTest("tree not clean vs HEAD for crates/: precondition absent")
        self.assertTrue("scanned" not in out or "nothing was verified" in out, out)

    def test_e4a1d386_shell_syntax_test_does_not_use_templateless_mktemp(self):
        src = (SCRIPTS / "tests" / "check-shell-syntax.sh").read_text()
        import re
        bad = re.findall(r"mktemp -d -t [A-Za-z0-9_-]+(?![A-Za-z0-9_.-])\)", src)
        self.assertEqual(bad, [], "GNU mktemp needs >=3 X's with -t")


class DirectInvocation(unittest.TestCase):
    """backlog 06a9d33f (FIXED): a scripts/test_*.py run directly must execute tests."""

    @staticmethod
    def runs_tests_when_executed(source):
        return "unittest.main" in source and "__main__" in source

    def test_every_scripts_test_file_has_a_main_guard_that_runs_unittest(self):
        offenders = [p.name for p in sorted(SCRIPTS.glob("test_*.py"))
                     if not self.runs_tests_when_executed(p.read_text())]
        self.assertEqual(offenders, [], "direct `python3 <file>` would run 0 tests and exit 0")

    def test_the_check_would_catch_a_file_without_a_guard(self):
        self.assertFalse(self.runs_tests_when_executed("import unittest\nclass T(unittest.TestCase):\n    def test_x(self): pass\n"))


if __name__ == "__main__":
    unittest.main()
