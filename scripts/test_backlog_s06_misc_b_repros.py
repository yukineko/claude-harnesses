"""Reproduction tests for pending backlog items (audit shard s06-misc-b).

Every test here asserts the CORRECT behaviour of something the named backlog item
says is broken, so each one FAILS while the defect is open. They are skipped by
default so the suite stays green; run them with

    RUN_BACKLOG_REPROS=1 python3 scripts/test_backlog_s06_misc_b_repros.py

and remove the matching skip decorator in the same commit that fixes the defect.
Stdlib only. Everything runs against throwaway repos / tmp dirs; nothing here
writes to the real repository.
"""

from __future__ import annotations

import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import types
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent
sys.path.insert(0, str(HERE))

RUN = bool(os.environ.get("RUN_BACKLOG_REPROS"))


def open_defect(item: str):
    return unittest.skipUnless(
        RUN, f"backlog {item}: open defect, set RUN_BACKLOG_REPROS=1 to run; remove skip when fixed"
    )


def _git_env():
    return dict(
        os.environ,
        GIT_CONFIG_GLOBAL=os.devnull,
        GIT_CONFIG_NOSYSTEM="1",
        GIT_AUTHOR_NAME="t",
        GIT_AUTHOR_EMAIL="t@example.invalid",
        GIT_COMMITTER_NAME="t",
        GIT_COMMITTER_EMAIL="t@example.invalid",
    )


def _git(cwd, *a):
    return subprocess.run(["git", *a], cwd=cwd, env=_git_env(), capture_output=True, text=True)


def _scratch_repo():
    d = Path(tempfile.mkdtemp(prefix="backlog-s06b-")).resolve()
    _git(d, "init", "-q", "-b", "main")
    return d


def _hook(script: str, payload, cwd=None, project=None):
    env = dict(os.environ)
    if project is not None:
        env["CLAUDE_PROJECT_DIR"] = str(project)
    stdin = payload if isinstance(payload, str) else json.dumps(payload)
    return subprocess.run(
        [sys.executable, str(HERE / script)], input=stdin, capture_output=True, text=True,
        cwd=str(cwd) if cwd else None, env=env,
    )


def _bash(cmd):
    return {"tool_name": "Bash", "tool_input": {"command": cmd}}


class Backlog05726f9f(unittest.TestCase):
    @open_defect("05726f9f")
    def test_no_script_test_loads_its_subject_through_stale_pyc_prone_exec_module(self):
        offenders = []
        for p in sorted(HERE.glob("test_*.py")):
            if p.name == Path(__file__).name:
                continue
            for line in p.read_text(encoding="utf-8").splitlines():
                code = line.split("#", 1)[0]
                if re.search(r"\.loader\.exec_module\(", code):
                    offenders.append(p.name)
                    break
        self.assertEqual(offenders, [], "these suites read their subject via SourceFileLoader (pyc cache)")


class Backlog_bee1ccbc(unittest.TestCase):
    # guard-maintree-edit.py half fixed by e033c406 (an unreadable payload
    # cannot be checked against the deny ledger, so it is refused).
    def test_guard_maintree_edit_refuses_unreadable_payload(self):
        for bad in ("this is not json\n", "[1,2]\n"):
            with self.subTest(payload=bad):
                self.assertEqual(_hook("guard-maintree-edit.py", bad).returncode, 2)

    @open_defect("bee1ccbc")
    def test_deny_no_verify_refuses_unreadable_payload(self):
        for bad in ("this is not json\n", "[1,2]\n"):
            with self.subTest(payload=bad):
                self.assertEqual(_hook("deny-no-verify.py", bad).returncode, 2)


class Backlog_e3d49aea(unittest.TestCase):
    @open_defect("e3d49aea")
    def test_edit_of_unmerged_file_is_allowed_during_a_merge_but_other_paths_stay_refused(self):
        d = _scratch_repo()
        self.addCleanup(shutil.rmtree, d, True)
        (d / "c.txt").write_text("base\n"); _git(d, "add", "-A"); _git(d, "commit", "-q", "-m", "base")
        _git(d, "checkout", "-q", "-b", "feat"); (d / "c.txt").write_text("feat\n"); _git(d, "commit", "-qam", "feat")
        _git(d, "checkout", "-q", "main"); (d / "c.txt").write_text("main\n"); _git(d, "commit", "-qam", "main")
        self.assertNotEqual(_git(d, "merge", "feat").returncode, 0)
        self.assertTrue((d / ".git" / "MERGE_HEAD").exists())

        def edit(path):
            return _hook("guard-maintree-edit.py",
                         {"tool_name": "Edit", "tool_input": {"file_path": str(path), "old_string": "a", "new_string": "b"}},
                         cwd=d, project=d).returncode
        self.assertEqual(edit(d / "c.txt"), 0, "the conflicted file must be editable to resolve the merge")
        self.assertEqual(edit(d / "other.txt"), 2, "any other path must stay refused")


class Backlog_911a93b8(unittest.TestCase):
    @open_defect("911a93b8")
    def test_python_test_assertion_loss_is_reported(self):
        d = _scratch_repo()
        self.addCleanup(shutil.rmtree, d, True)
        p = d / "scripts" / "test_gate.py"
        p.parent.mkdir()
        p.write_text("import unittest\nclass T(unittest.TestCase):\n    def test_a(self):\n"
                     "        self.assertEqual(f(), 1)\n        self.assertTrue(g())\n        self.assertIn('x', h())\n")
        _git(d, "add", "-A"); _git(d, "commit", "-q", "-m", "base")
        p.write_text("import unittest\nclass T(unittest.TestCase):\n    def test_a(self):\n        pass\n")
        r = subprocess.run([sys.executable, str(HERE / "check-test-weakening.py"), "--repo", str(d), "--base", "HEAD", "--json"],
                           capture_output=True, text=True, cwd=d)
        self.assertNotEqual(r.returncode, 0, r.stdout)


class Backlog_df7acf51(unittest.TestCase):
    @open_defect("df7acf51")
    def test_checker_does_not_report_removable_a_dir_the_pruner_keeps(self):
        import plugin_cache
        # exec(compile(...)) rather than SourceFileLoader: see backlog 05726f9f (stale pyc).
        src_path = HERE / "check-plugin-rollout.py"
        cpr = types.ModuleType("cpr_s06b")
        cpr.__file__ = str(src_path)
        exec(compile(src_path.read_text(encoding="utf-8"), str(src_path), "exec"), cpr.__dict__)
        root = Path(tempfile.mkdtemp(prefix="backlog-s06b-cache-")).resolve()
        self.addCleanup(shutil.rmtree, root, True)
        pj = root / "crates" / "foo" / ".claude-plugin"
        pj.mkdir(parents=True)
        (pj / "plugin.json").write_text(json.dumps({"name": "foo", "version": "2.0.0"}))
        cache = root / "cache" / "yukineko"
        for v in ("1.0.0", "2.0.0"):
            (cache / "foo" / v).mkdir(parents=True)
        reg = root / "installed_plugins.json"
        reg.write_text(json.dumps({"version": 1, "plugins": {"foo@yukineko": [
            {"scope": "user", "installPath": str(cache / "foo" / "1.0.0"), "version": "1.0.0"}]}}))
        current, _ = plugin_cache.source_versions(str(root / "crates"))
        pins, pu = plugin_cache.settings_pinned_versions(str(cache))
        refs, ru = plugin_cache.registry_referenced_versions(str(cache), str(reg))
        stale, _ = plugin_cache.scan(str(cache), current, settings_pins=pins, settings_undetermined=pu,
                                     registry_refs=refs, registry_undetermined=ru)
        self.assertEqual([s.removable for s in stale], [False], "precondition: the pruner keeps the registry-pointed dir")
        cpr.PLUGIN_CACHE_ROOT = str(cache)
        cpr.CRATES = str(root / "crates")
        problems, _ = cpr.check_stale_version_dirs()
        self.assertEqual(problems, [], "the checker reports a dir as removable that the pruner will not delete")


class Backlog_ae8e917b(unittest.TestCase):
    @open_defect("ae8e917b")
    def test_known_open_bypass_shapes_are_refused(self):
        for cmd in ("echo `git commit -n -m x`", "env -S 'git commit --no-verify -m x'"):
            with self.subTest(cmd=cmd):
                self.assertEqual(_hook("deny-no-verify.py", _bash(cmd)).returncode, 2)


class Backlog_25692f6e(unittest.TestCase):
    @open_defect("25692f6e")
    def test_post_commit_does_not_advertise_a_nonexistent_status_flag(self):
        r = subprocess.run([sys.executable, str(HERE / "gate-bypass.py"), "--status"], capture_output=True, text=True)
        hook = (REPO / ".githooks" / "post-commit").read_text()
        advertised = "gate-bypass.py --status" in hook
        self.assertFalse(advertised and r.returncode == 2 and "unrecognized arguments" in r.stderr,
                         "post-commit tells the reader to run a flag gate-bypass.py rejects")


def _patched_gate_harness():
    import test_gate_bypass as T
    S = T.SCANNERS
    for after, new in (("check-fail-open.py", "check-fail-open-diff.py"),
                       ("check-raw-io-ratchet.py", "check-fault-injection-adoption.py"),
                       ("check-gate-crates-sync.py", "check-gate-protection.py")):
        if new not in S:
            S.insert(S.index(after) + 1, new)
    return T


class LedgerRepros(unittest.TestCase):
    @open_defect("3d97dd01")
    def test_ff_onto_an_already_gated_merge_commit_is_not_recorded_as_ungated(self):
        T = _patched_gate_harness()
        h = T.GateHarness()
        self.addCleanup(h.cleanup)
        h.write("a.txt", "a\n"); h.git("add", "-A"); self.assertEqual(h.commit("c1").returncode, 0)
        h.git("branch", "wt1"); h.git("branch", "wt2")
        h.git("checkout", "-q", "-b", "feat"); h.write("f.txt", "f\n"); h.git("add", "-A"); h.commit("feat")
        h.git("checkout", "-q", "main"); h.write("m.txt", "m\n"); h.git("add", "-A"); h.commit("main2")
        self.assertEqual(h.git("merge", "--no-ff", "-q", "-m", "merge feat", "feat", check=False).returncode, 0)
        self.assertEqual(h.ledger_lines(), [], "precondition: the gated merge on main is not recorded")
        wts = []
        for n in ("wt1", "wt2"):
            w = h.root.parent / (h.root.name + "-" + n)
            wts.append(w)
            h.git("worktree", "add", "-q", str(w), n)
            self.addCleanup(shutil.rmtree, w, True)
            self.assertEqual(h.git("merge", "--ff-only", "-q", "main", cwd=w, check=False).returncode, 0)
        self.assertEqual(h.ledger_lines(), [], "ff of an already-inspected merge commit must not enter the ledger")

    @open_defect("a64c77a3")
    def test_concurrent_postcommit_append_survives_precommit_ledger_rewrite(self):
        T = _patched_gate_harness()
        h = T.GateHarness()
        self.addCleanup(h.cleanup)
        h.write("a.txt", "a\n"); h.git("add", "-A")
        self.assertEqual(h.commit("ungated", "--no-verify").returncode, 0)
        self.assertEqual(len(h.ledger_lines()), 1)
        h.write("b.txt", "b\n"); h.git("add", "-A")
        shim = h.root / ".stub-bin" / "rm"
        shim.write_text('#!/bin/sh\nfor a in "$@"; do case "$a" in */gate-bypassed) '
                        'printf "%s\\t%s\\n" "1111111111111111111111111111111111111111" "concurrent-session-entry" >> "$a";; esac; done\n'
                        'exec ' + shutil.which("rm") + ' "$@"\n')
        shim.chmod(0o755)
        self.assertEqual(h.commit("gated").returncode, 0)
        self.assertTrue(any("concurrent-session-entry" in l for l in h.ledger_lines()),
                        "an entry appended between pre-commit's read and its rm/mv was lost")


if __name__ == "__main__":
    unittest.main(verbosity=2)
