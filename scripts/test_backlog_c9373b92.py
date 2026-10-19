#!/usr/bin/env python3
"""Repro for backlog c9373b92 (duplicate 5e54fb25): `scripts/rollout-plugins.sh
--plugin <name>` skips verify_rollout_complete() entirely, so it returns 0 while
the very plugin it rolled out is still drifted (including "installed but execs
nothing"). Measured 2026-09-13: `--plugin tdd` rc=0 while
`check-plugin-rollout.py` rc=1 for tdd drift afterwards.

The right fix is a SCOPED verification, not a skip. Contract these tests pin:

  1. check-plugin-rollout.py gains a repeatable `--only <name>` (plugin name or
     crate dir). ROLLOUT-class findings prefixed "<plugin>: " that belong to a
     KNOWN plugin NOT named by --only are out of scope (reported, not fatal).
     Findings for the named plugins, and findings attributable to no known
     plugin, stay fatal (rc 1). An --only name matching no plugin is rc 1.
     Without --only, behaviour is unchanged.
  2. verify_rollout_complete(), for a filtered run, runs the checker with
     `--only <name>` per targeted plugin and treats rc 1 as fatal. It no longer
     prints "verify: skipped" for a filtered run.

Nothing here runs rollout-plugins.sh or touches ~/.claude: part A lifts
verify_rollout_complete() verbatim into a bash sandbox with a stub checker; part
B runs the REAL checker as a subprocess against a fixture fleet under a temp
dir with every path override set (registry, settings, cache, parked, retired,
HOME), and a real throwaway git repo so the provenance dimension is determined.
Stdlib only (the fixture builder is reused from test_check_plugin_rollout.py).
"""
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
SCRIPT = HERE / "rollout-plugins.sh"
CHECKER = HERE / "check-plugin-rollout.py"
BASH = "/bin/bash" if os.path.exists("/bin/bash") else shutil.which("bash")

import test_check_plugin_rollout as T  # noqa: E402  (fixture builder)


def verify_fn() -> str:
    src = SCRIPT.read_text(encoding="utf-8")
    m = re.search(r"^verify_rollout_complete\(\) \{\n.*?^\}\n", src, re.S | re.M)
    if not m:
        raise AssertionError("verify_rollout_complete() not found -- undetermined, not clean")
    return m.group(0)


class VerifyRolloutCompleteScoped(unittest.TestCase):
    """Part A: the shell function, with a stub checker recording its argv."""

    def call(self, only_plugins, stub_rc):
        with tempfile.TemporaryDirectory() as t:
            t = Path(t).resolve()
            (t / "scripts").mkdir()
            marker = t / "argv"
            (t / "scripts" / "check-plugin-rollout.py").write_text(
                "import sys\n"
                f"open({str(marker)!r}, 'a').write(' '.join(sys.argv[1:]) + '\\n')\n"
                "print('ROLLOUT DRIFT: p: stub')\n"
                f"sys.exit({stub_rc})\n")
            prog = (f"set -u\nREPO='{t}'\ndry=0\nonly_plugins=({only_plugins})\n"
                    + verify_fn() + "\nverify_rollout_complete\necho \"rc=$?\"\n")
            r = subprocess.run([BASH, "-c", prog], capture_output=True, text=True)
            out = r.stdout + r.stderr
            m = re.search(r"rc=(\d+)", r.stdout)
            argv = marker.read_text() if marker.exists() else None
            return (int(m.group(1)) if m else None), out, argv

    def test_control_unfiltered_run_with_drift_fails(self):
        rc, out, argv = self.call("", 1)
        self.assertIsNotNone(argv, "unfiltered run never invoked the checker:\n" + out)
        self.assertIsNotNone(rc, out)
        self.assertNotEqual(rc, 0, out)

    def test_filtered_run_with_failing_checker_is_fatal_and_scoped(self):
        rc, out, argv = self.call("p", 1)
        self.assertIsNotNone(argv, "a --plugin run never invoked the rollout checker:\n" + out)
        self.assertIn("--only p", argv, f"checker not scoped to the targeted plugin; argv={argv!r}")
        self.assertIsNotNone(rc, out)
        self.assertNotEqual(rc, 0, f"filtered run returned 0 though the checker exited 1:\n{out}")

    def test_filtered_run_with_clean_checker_succeeds_via_scoped_check(self):
        rc, out, argv = self.call("p", 0)
        self.assertIsNotNone(argv, "a --plugin run skipped the checker instead of scoping it:\n" + out)
        self.assertIn("--only p", argv, f"argv={argv!r}")
        self.assertEqual(rc, 0, out)
        self.assertNotIn("verify: skipped", out)

    def test_filtered_run_scopes_every_targeted_plugin(self):
        rc, out, argv = self.call("p q", 0)
        self.assertIsNotNone(argv, out)
        self.assertIn("--only p", argv)
        self.assertIn("--only q", argv)


class RealCheckerOnly(unittest.TestCase):
    """Part B: the real checker, sandboxed, one drifted plugin (benchkit)."""

    A, B = "condukt", "benchkit"  # both non-gate; B is the drifted one

    def build(self, drift):
        t = Path(tempfile.mkdtemp(prefix="c9373b92-")).resolve()
        self.addCleanup(shutil.rmtree, t, ignore_errors=True)
        kw = {}
        if drift:
            reg = dict(T.FIXTURE_PLUGINS)
            reg[self.B] = "0.0.9"
            kw["registry_versions"] = reg
        T._make_fixture(t, **kw)
        # A real git repo so the provenance dimension is DETERMINED (otherwise
        # every plugin reports "could not determine whether source moved" and
        # the drift dimension under test is drowned out).
        genv = dict(os.environ, GIT_CONFIG_GLOBAL="/dev/null", GIT_CONFIG_SYSTEM="/dev/null",
                    GIT_AUTHOR_NAME="t", GIT_AUTHOR_EMAIL="t@t", GIT_COMMITTER_NAME="t",
                    GIT_COMMITTER_EMAIL="t@t")

        def git(*a):
            r = subprocess.run(["git", *a], cwd=t, env=genv, capture_output=True, text=True)
            self.assertEqual(r.returncode, 0, f"git {a}: {r.stderr}")
            return r.stdout.strip()

        git("init", "-q")
        git("add", "crates")
        git("commit", "-q", "-m", "fixture")
        head = git("rev-parse", "HEAD")
        for mf in (t / "cache").rglob(".deployed-from.json"):
            mf.write_text(json.dumps({"commit": head, "dirty": False, "deployed_at": 0}))
        return t

    def run_checker(self, t, *only):
        env = dict(os.environ, HOME=str(t / "home"),
                   CLAUDE_PLUGIN_REGISTRY=str(t / "installed_plugins.json"),
                   CLAUDE_SETTINGS=str(t / "settings.json"),
                   CLAUDE_SETTINGS_JSON=str(t / "settings.json"),
                   CLAUDE_PLUGIN_CACHE=str(t / "cache"),
                   PARKED_PLUGINS=str(t / "parked.json"),
                   RETIRED_PLUGINS=str(t / "retired.json"))
        argv = [sys.executable, str(CHECKER)]
        for n in only:
            argv += ["--only", n]
        p = subprocess.run(argv, cwd=t, env=env, capture_output=True, text=True, timeout=120)
        return p.returncode, p.stdout + p.stderr

    def drift_line(self, out):
        return f"{self.B}: source=0.1.0 registry=0.0.9"

    # --- anti-vacuity controls: pass today and must keep passing ---
    def test_control_clean_fleet_is_rc0(self):
        t = self.build(drift=False)
        rc, out = self.run_checker(t)
        self.assertEqual(rc, 0, "sandbox is not clean without drift, so the cases below prove nothing:\n" + out)

    def test_control_no_only_with_drift_is_rc1(self):
        t = self.build(drift=True)
        rc, out = self.run_checker(t)
        self.assertEqual(rc, 1, out)
        self.assertIn(self.drift_line(out), out)

    # --- the contract ---
    def test_only_the_drifted_plugin_is_rc1(self):
        t = self.build(drift=True)
        rc, out = self.run_checker(t, self.B)
        self.assertEqual(rc, 1, out)
        self.assertIn(self.drift_line(out), out)

    def test_only_a_clean_plugin_does_not_fail_for_anothers_drift(self):
        t = self.build(drift=True)
        rc, out = self.run_checker(t, self.A)
        self.assertNotEqual(rc, 1, f"--only {self.A} failed on {self.B}'s drift (out of scope):\n{out}")
        self.assertIn(self.drift_line(out), out, "out-of-scope drift must still be REPORTED, not hidden")

    def test_only_unknown_plugin_is_rc1(self):
        t = self.build(drift=False)
        rc, out = self.run_checker(t, "nosuchplugin")
        self.assertEqual(rc, 1, "cannot verify an unknown plugin; that is not clean:\n" + out)

    def test_only_accepts_repeats_and_keeps_drifted_one_fatal(self):
        t = self.build(drift=True)
        rc, out = self.run_checker(t, self.A, self.B)
        self.assertEqual(rc, 1, out)


if __name__ == "__main__":
    unittest.main()
