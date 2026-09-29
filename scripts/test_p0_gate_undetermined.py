#!/usr/bin/env python3
"""RED tests for backlog 2cef09c5 and c767cb47 (gate "cannot determine" -> allow).

Stdlib-only.  Every case runs the real script (or hook) against a THROWAWAY git
repository; nothing touches the real repository.

    python3 scripts/test_p0_gate_undetermined.py

Written by the test author, not the implementer.  Each "control" test pins the
behaviour that must NOT change, so a RED in the sibling test cannot be an
artefact of a broken fixture.

  2cef09c5  check-version-bumped.py : base blob unreadable/corrupt -> must not be
            read as "new plugin" (exit 0).  Shared-crate arm: unreadable base
            manifest -> must not be "ok".
            check-plugin-versions.py : zero plugins checked -> must not be exit 0.
            deny-no-verify.py : `sh -c`, `env ...` wrappers must be refused like
            the bare form.
  c767cb47  .githooks/pre-commit clears the whole shared bypass ledger on ANY
            green run, including an empty staged diff and a run from a worktree
            whose history does not contain the ungated commit.
"""
from __future__ import annotations

import importlib.util
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

_HERE = Path(__file__).resolve().parent
_SCRIPTS = _HERE
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


class Repo:
    def __init__(self):
        self.root = Path(tempfile.mkdtemp(prefix="p0gate-")).resolve()
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

    def run(self, script, *args):
        return subprocess.run(
            [sys.executable, str(_SCRIPTS / script), *args],
            cwd=str(self.root), env=_ENV, capture_output=True, text=True)


def _plugin(repo, name, version, extra_src=None):
    repo.write("crates/%s/.claude-plugin/plugin.json" % name,
               json.dumps({"name": name, "version": version}))
    repo.write("crates/%s/Cargo.toml" % name,
               '[package]\nname = "%s"\nversion = "%s"\n' % (name, version))
    repo.write("crates/%s/src/lib.rs" % name, extra_src or "// v1\n")


class Base(unittest.TestCase):
    def repo(self):
        r = Repo()
        self.addCleanup(r.cleanup)
        return r


class VersionBumpedBaseUnreadable(Base):
    """2cef09c5 (1): plugin_version_at() returns None for absent / git-show
    failure / corrupt JSON, and the consumer reads all three as 'new plugin'."""

    def test_control_touched_plugin_without_bump_is_rc1(self):
        r = self.repo(); _plugin(r, "blastguard", "1.0.0"); r.commit_all()
        r.write("crates/blastguard/src/lib.rs", "// changed\n")
        self.assertEqual(r.run("check-version-bumped.py").returncode, 1)

    def test_control_touched_plugin_with_bump_is_rc0(self):
        r = self.repo(); _plugin(r, "blastguard", "1.0.0"); r.commit_all()
        r.write("crates/blastguard/src/lib.rs", "// changed\n")
        _plugin(r, "blastguard", "1.0.1", "// changed\n")
        self.assertEqual(r.run("check-version-bumped.py").returncode, 0)

    def test_control_plugin_genuinely_absent_at_base_is_rc0(self):
        r = self.repo(); r.write("README", "x\n"); r.commit_all()
        _plugin(r, "newplug", "0.1.0")
        _git(r.root, "add", "-A")
        self.assertEqual(r.run("check-version-bumped.py").returncode, 0)

    def test_corrupt_plugin_json_at_base_is_not_a_new_plugin(self):
        r = self.repo(); _plugin(r, "blastguard", "1.0.0")
        r.write("crates/blastguard/.claude-plugin/plugin.json", "{not json")
        r.commit_all()
        # working tree: valid manifest, SAME version, source changed
        _plugin(r, "blastguard", "1.0.0", "// changed\n")
        p = r.run("check-version-bumped.py")
        self.assertNotEqual(
            p.returncode, 0,
            "base plugin.json is corrupt -> 'cannot determine' must not exit 0; "
            "stdout=%r" % p.stdout)

    def test_base_plugin_json_without_version_field_is_not_a_new_plugin(self):
        r = self.repo(); _plugin(r, "blastguard", "1.0.0")
        r.write("crates/blastguard/.claude-plugin/plugin.json", '{"name":"blastguard"}')
        r.commit_all()
        _plugin(r, "blastguard", "1.0.0", "// changed\n")
        p = r.run("check-version-bumped.py")
        self.assertNotEqual(p.returncode, 0,
                            "plugin existed at base but its version is unknown; "
                            "stdout=%r" % p.stdout)


class VersionBumpedSharedCrateBaseUnreadable(Base):
    """2cef09c5 (2): shared-crate arm returns ok when the base manifest cannot
    be read, though 'absent at base' (new crate) is distinguishable."""

    def test_control_shared_crate_absent_at_base_is_rc0(self):
        r = self.repo(); r.write("README", "x\n"); r.commit_all()
        r.write("crates/harness-core/Cargo.toml",
                '[package]\nname = "harness-core"\nversion = "0.1.0"\n')
        r.write("crates/harness-core/src/lib.rs", "// new\n")
        _git(r.root, "add", "-A")
        self.assertEqual(r.run("check-version-bumped.py").returncode, 0)

    def test_control_shared_crate_changed_without_bump_is_rc1(self):
        r = self.repo()
        r.write("crates/harness-core/Cargo.toml",
                '[package]\nname = "harness-core"\nversion = "0.1.0"\n')
        r.write("crates/harness-core/src/lib.rs", "// a\n"); r.commit_all()
        r.write("crates/harness-core/src/lib.rs", "// b\n")
        self.assertEqual(r.run("check-version-bumped.py").returncode, 1)

    def test_corrupt_manifest_at_base_with_linked_change_is_not_ok(self):
        r = self.repo()
        r.write("crates/harness-core/Cargo.toml", "this is not toml [[[\n")
        r.write("crates/harness-core/src/lib.rs", "// a\n"); r.commit_all()
        r.write("crates/harness-core/Cargo.toml",
                '[package]\nname = "harness-core"\nversion = "0.1.0"\n')
        r.write("crates/harness-core/src/lib.rs", "// b\n")
        p = r.run("check-version-bumped.py")
        self.assertNotEqual(
            p.returncode, 0,
            "base manifest exists but is unreadable; that is not 'new crate'. "
            "stdout=%r" % p.stdout)


class PluginVersionsEmptySet(Base):
    """2cef09c5 (3): an empty checked set exits 0 'OK: 0 plugins'."""

    def test_control_consistent_plugin_is_rc0(self):
        r = self.repo(); _plugin(r, "p1", "1.0.0")
        r.write(".claude-plugin/marketplace.json",
                json.dumps({"plugins": [{"name": "p1", "version": "1.0.0"}]}))
        self.assertEqual(r.run("check-plugin-versions.py").returncode, 0)

    def test_control_drift_is_rc1(self):
        r = self.repo(); _plugin(r, "p1", "1.0.0")
        r.write(".claude-plugin/marketplace.json",
                json.dumps({"plugins": [{"name": "p1", "version": "9.9.9"}]}))
        self.assertEqual(r.run("check-plugin-versions.py").returncode, 1)

    def test_zero_plugins_checked_is_not_ok(self):
        r = self.repo()
        (r.root / "crates").mkdir()
        r.write(".claude-plugin/marketplace.json", json.dumps({"plugins": []}))
        p = r.run("check-plugin-versions.py")
        self.assertNotIn(p.returncode, (0,),
                         "nothing was checked; must not report success: %r" % p.stdout)

    def test_crates_with_no_plugin_json_at_all_is_not_ok(self):
        r = self.repo()
        r.write("crates/foo/Cargo.toml", '[package]\nname="foo"\nversion="1.0.0"\n')
        r.write(".claude-plugin/marketplace.json", json.dumps({"plugins": []}))
        p = r.run("check-plugin-versions.py")
        self.assertNotEqual(p.returncode, 0, p.stdout)


class DenyNoVerifyWrapped(unittest.TestCase):
    """2cef09c5 (4): wrapped forms of the bypass are allowed (rc 0) while the
    bare form is refused (rc 2)."""

    def hook(self, command):
        return subprocess.run(
            [sys.executable, str(_SCRIPTS / "deny-no-verify.py")],
            input=json.dumps({"tool_name": "Bash",
                              "tool_input": {"command": command}}),
            capture_output=True, text=True)

    def test_control_bare_form_denied(self):
        self.assertEqual(self.hook("git commit --no-verify -m x").returncode, 2)

    def test_control_benign_wrapped_forms_allowed(self):
        for c in ('sh -c "git commit -m x"', "env FOO=1 git commit -m x",
                  "bash -c 'echo hi'"):
            with self.subTest(command=c):
                self.assertEqual(self.hook(c).returncode, 0)

    def test_wrapped_forms_are_denied_like_the_bare_form(self):
        for c in (
            'sh -c "git commit --no-verify"',
            "env FOO=1 git commit --no-verify",
            "bash -lc 'git commit --no-verify -m ok'",
            "eval 'git commit --no-verify -m ok'",
            "env git commit --no-verify -m ok",
            "nohup git commit --no-verify -m ok",
            "time git commit --no-verify -m ok",
        ):
            with self.subTest(command=c):
                self.assertEqual(self.hook(c).returncode, 2,
                                 "wrapped bypass allowed: %r" % c)


# --------------------------------------------------------------------------
# c767cb47: drives the real .githooks/pre-commit through test_gate_bypass's
# harness (stub scanners, throwaway repo).  The clearing logic lives ONLY in
# .githooks/pre-commit (lines ~346-353); there is no script-level seam, so the
# fix needs a human-applied .githooks patch (.githooks is agent-edit-denied).
# --------------------------------------------------------------------------
_spec = importlib.util.spec_from_file_location(
    "tgb", _HERE / "test_gate_bypass.py")
tgb = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(tgb)


class LedgerClearedOnlyByInspection(tgb.GateTestCase):
    def _bypass_one(self, h):
        h.write("a.txt", "ungated\n")
        h.git("add", "-A")
        h.commit("bypassed", "--no-verify")
        self.assertLedgerHas(h, 1)

    def test_control_green_run_with_a_real_staged_diff_clears(self):
        # existing contract (GreenRunClearsTheLedger) - must survive the fix
        h = self.harness()
        self._bypass_one(h)
        h.write("b.txt", "gated\n")
        h.git("add", "-A")
        self.assertEqual(h.pre_commit().returncode, 0)
        self.assertLedgerEmpty(h, "after a green run over a real diff")

    def test_control_empty_diff_run_is_green_and_ledger_empty_stays_empty(self):
        h = self.harness()
        self.assertEqual(h.pre_commit().returncode, 0)
        self.assertLedgerEmpty(h)

    def test_empty_staged_diff_does_not_launder_an_ungated_commit(self):
        """Observed live 2026-09-24: an EMPTY staged diff printed 'cleared 1
        ungated commit(s)'.  Nothing was staged, so nothing was inspected that
        could cover the ungated commit's diff."""
        h = self.harness()
        self._bypass_one(h)
        self.assertEqual(h.git("diff", "--cached", "--name-only").stdout.strip(), "",
                         "precondition: staged diff is empty")
        proc = h.pre_commit()
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertLedgerHas(h, 1, "an empty-diff run must not clear the ledger; "
                                   "stderr=%r" % proc.stderr)

    def test_run_from_a_worktree_lacking_the_ungated_commit_does_not_clear(self):
        """The ledger is repo-wide (git-common-dir) but a green run in ANY
        worktree deletes it, including for commits that worktree's history
        never contained and whose diff it therefore never saw."""
        h = self.harness()
        base = h.git("rev-parse", "HEAD").stdout.strip()
        self._bypass_one(h)  # ungated commit X on main
        wt = h.root.parent / (h.root.name + "-wt")
        self.addCleanup(shutil.rmtree, wt, True)
        h.git("worktree", "add", "-q", "-b", "other", str(wt), base)
        # sanity: X is not reachable from the other worktree
        x = h.ledger_lines()[0].split("\t")[0]
        self.assertNotEqual(
            h.git("merge-base", "--is-ancestor", x, "other", check=False).returncode, 0,
            "precondition: X is not in the other worktree's history")
        h.write("c.txt", "other work\n", cwd=wt)
        h.git("add", "c.txt", cwd=wt)
        proc = h.pre_commit(cwd=wt)
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertLedgerHas(h, 1, "a green run in a worktree that never saw X "
                                   "must not clear X; stderr=%r" % proc.stderr)


if __name__ == "__main__":
    unittest.main(verbosity=2)
