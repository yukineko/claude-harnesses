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


class MainAndWorktree:
    """A throwaway main checkout plus a sibling linked worktree."""

    def __init__(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="s05-")).resolve()
        self.main = self.tmp / "main"
        self.wt = self.tmp / "wt"
        self.main.mkdir()
        _git(self.main, "init", "-q", "-b", "main")
        (self.main / "f.txt").write_text("a\n")
        _git(self.main, "add", "f.txt")
        _git(self.main, "commit", "-q", "-m", "seed")
        _git(self.main, "worktree", "add", "-q", "-b", "feat", str(self.wt))
        (self.wt / ".scratch").mkdir()

    def close(self):
        shutil.rmtree(self.tmp, ignore_errors=True)

    def bash_guard(self, command, cwd=None):
        payload = {"tool_name": "Bash", "tool_input": {"command": command}}
        r = subprocess.run(
            [sys.executable, str(SCRIPTS / "guard-maintree-bash.py")],
            input=json.dumps(payload), capture_output=True, text=True,
            cwd=str(cwd or self.wt), env=_env(CLAUDE_PROJECT_DIR=str(self.main)),
        )
        return r.returncode, r.stderr


class BashGuard(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.w = MainAndWorktree()

    @classmethod
    def tearDownClass(cls):
        cls.w.close()

    # --- controls (must hold today) -----------------------------------
    def test_control_redirect_into_main_is_refused(self):
        rc, _ = self.w.bash_guard(f"echo hi > {self.w.main}/p.txt")
        self.assertEqual(rc, 2)

    def test_control_cp_from_worktree_into_main_is_refused(self):
        rc, _ = self.w.bash_guard(f"cp {self.w.wt}/f.txt {self.w.main}/g.txt")
        self.assertEqual(rc, 2)

    def test_control_cp_from_tmp_into_worktree_is_allowed(self):
        rc, err = self.w.bash_guard(f"cp /etc/hosts {self.w.wt}/.scratch/h")
        self.assertEqual(rc, 0, err)

    # --- open defects --------------------------------------------------
    @open_defect("b5358f58")
    def test_b5358f58_cp_reading_from_main_into_worktree_is_allowed(self):
        rc, err = self.w.bash_guard(f"cp {self.w.main}/f.txt {self.w.wt}/f.copy")
        self.assertEqual(rc, 0, "read-only source operand judged as a write target: " + err[:200])

    @open_defect("ad524af9")
    def test_ad524af9_fd_redirect_does_not_make_a_stray_target(self):
        rc, err = self.w.bash_guard(f"cp /etc/hosts {self.w.wt}/.scratch/h 2>/dev/null")
        self.assertEqual(rc, 0, "fd number '2' treated as an operand: " + err[:200])

    @open_defect("cc809395")
    def test_cc809395_relative_paths_resolve_against_cwd_not_main(self):
        rc, err = self.w.bash_guard("sed -i '' 's/a/b/' f.txt", cwd=self.w.wt)
        self.assertEqual(rc, 0, "relative operand resolved under main root: " + err[:200])

    @open_defect("ae4543d5")
    def test_ae4543d5_interpreter_wrapper_write_into_main_is_refused(self):
        for cmd in (
            f"sh -c 'echo hi > {self.w.main}/p.txt'",
            f"python3 -c \"open('{self.w.main}/p.txt','w').write('x')\"",
            f"cd /tmp && echo hi > {self.w.main}/p.txt",
            f"perl -pi -e 's/a/b/' {self.w.main}/f.txt",
            f"sed --in-place 's/a/b/' {self.w.main}/f.txt",
        ):
            with self.subTest(cmd=cmd):
                rc, _ = self.w.bash_guard(cmd)
                self.assertEqual(rc, 2, "same-effect write into main was allowed")


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


class DeadlockOnUntracked(unittest.TestCase):
    @open_defect("d796a630")
    def test_d796a630_main_with_only_an_untracked_file_has_an_exit(self):
        """The three gates must not form a cycle: if the stop hook blocks on an
        untracked file that `git merge` refuses to overwrite, the guard must let
        the byte-identical untracked file be removed."""
        tmp = Path(tempfile.mkdtemp(prefix="s05d-")).resolve()
        try:
            _git(tmp, "init", "-q", "-b", "main")
            (tmp / "a").write_text("base\n")
            _git(tmp, "add", "a")
            _git(tmp, "commit", "-q", "-m", "base")
            _git(tmp, "checkout", "-q", "-b", "feat")
            (tmp / "docs").mkdir()
            (tmp / "docs" / "spec.md").write_text("spec\n")
            _git(tmp, "add", "docs")
            _git(tmp, "commit", "-q", "-m", "spec")
            _git(tmp, "checkout", "-q", "main")
            (tmp / "docs").mkdir()
            (tmp / "docs" / "spec.md").write_text("spec\n")
            stop = subprocess.run(
                [sys.executable, str(SCRIPTS / "stop-verify-worktree.py")],
                input=json.dumps({"cwd": str(tmp)}), capture_output=True, text=True, env=_env(),
            ).returncode
            merge = subprocess.run(["git", "merge", "feat"], cwd=tmp, env=_env(), capture_output=True, text=True).returncode
            rm = subprocess.run(
                [sys.executable, str(SCRIPTS / "guard-maintree-bash.py")],
                input=json.dumps({"tool_name": "Bash", "tool_input": {"command": f"rm {tmp}/docs/spec.md"}}),
                capture_output=True, text=True, env=_env(CLAUDE_PROJECT_DIR=str(tmp)),
            ).returncode
            # cycle = stop blocks AND merge refused AND rm refused
            self.assertFalse(stop == 2 and merge != 0 and rm == 2,
                             f"deadlock: stop={stop} merge={merge} rm={rm}")
        finally:
            shutil.rmtree(tmp, ignore_errors=True)


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

    @open_defect("9b64f427")
    def test_9b64f427_1_dangling_cache_root_is_a_problem(self):
        root = self.tmp / "cache"
        os.symlink(self.tmp / "nowhere", root)
        stale, problems = self.pc.scan(str(root), {"alpha": "1.0.0"})
        self.assertTrue(problems, "dangling cache root resolved to clean")

    @open_defect("9b64f427")
    def test_9b64f427_2_unrecognised_in_use_marker_holds_the_dir(self):
        d = self.tmp / "c" / "alpha" / "0.9.0" / ".in_use"
        d.mkdir(parents=True)
        (d / "pid-12345").write_text("")
        stale, problems = self.pc.scan(str(self.tmp / "c"), {"alpha": "1.0.0"})
        self.assertFalse(any(s.removable for s in stale) and not problems,
                         "unrecognised holder record read as 'nobody holds it'")

    @open_defect("9b64f427")
    def test_9b64f427_3_unreadable_crate_is_a_problem(self):
        pj = self.tmp / "crates" / "alpha" / ".claude-plugin" / "plugin.json"
        pj.parent.mkdir(parents=True)
        os.symlink(self.tmp / "gone.json", pj)
        versions, problems = self.pc.source_versions(str(self.tmp / "crates"))
        self.assertTrue(problems or "alpha" in versions, "unreadable crate silently dropped")

    @open_defect("9b64f427")
    def test_9b64f427_4_non_version_directory_is_not_removable(self):
        (self.tmp / "c" / "alpha" / "node_modules").mkdir(parents=True)
        stale, problems = self.pc.scan(str(self.tmp / "c"), {"alpha": "1.0.0"})
        self.assertFalse(any(s.removable for s in stale), "non-version dir handed to rmtree")


class RolloutProvenance(unittest.TestCase):
    @open_defect("649d15f6")
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
    @open_defect("f6919056")
    def test_f6919056_check_versions_without_crates_dir_is_not_ok(self):
        tmp = Path(tempfile.mkdtemp(prefix="s05v-")).resolve()
        try:
            (tmp / "scripts").mkdir()
            shutil.copy2(SCRIPTS / "check-versions.sh", tmp / "scripts" / "check-versions.sh")
            r = subprocess.run(["bash", str(tmp / "scripts" / "check-versions.sh")], capture_output=True, text=True)
            self.assertNotEqual(r.returncode, 0, "parity 'OK' with zero crates inspected: " + r.stdout)
        finally:
            shutil.rmtree(tmp, ignore_errors=True)

    @open_defect("f6919056")
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

    @open_defect("e4a1d386")
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
