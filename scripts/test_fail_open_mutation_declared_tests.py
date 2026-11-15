#!/usr/bin/env python3
"""Narrowed mutation tests: each Scenario in scripts/check-fail-open-mutation.py
declares which cargo test targets must catch its mutation (`Scenario.tests`),
and the script runs exactly those instead of the whole crate.

Run: `python3 scripts/test_fail_open_mutation_declared_tests.py` (stdlib unittest).
cargo is a PATH stub (like test_fail_open_mutation_mutates_invoking_tree_20261113.py);
the real script runs inside a scratch git repo. No real cargo test is run.

  b1  static: every SCENARIOS entry has a non-empty `tests` of strings that are
      only `--lib`, `--test <name>` or `--bin <crate>` (each `--test <name>`
      names an existing crates/<crate>/tests/<name>.rs; `--bin` only the crate's
      own src/main.rs binary — added by the implementer for specguard, which
      has no tests/ dir).                            (RED at origin/main: no attr)
  b2  end-to-end: the argv cargo sees for the test run is `test -p <crate>`
      followed by exactly the scenario's declared selectors (an optional
      trailing `-- ...` harness-args tail is tolerated). (RED at origin/main)
  b3  anti-weakening: cargo exit 0 + `test result: ok` => script exits non-zero
      (NOT CAUGHT).                                  (GUARD at origin/main)
  b4  anti-weakening: cargo `error: no test target named ...`, exit 101, no
      `test result:` line => script exits non-zero (a rejected selector is not
      a caught RED).                                 (GUARD at origin/main)

DOES NOT PROVE: that a declared test target really catches its mutation (that
needs real cargo), that selectors are minimal, or anything about the other six
scenarios' end-to-end behaviour (b2-b4 use the blastguard scenario only; b1
covers all statically). b1 does not check that a `--test` file contains tests.
"""

from __future__ import annotations

import importlib.util
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent
SCRIPT = HERE / "check-fail-open-mutation.py"


def _load():
    spec = importlib.util.spec_from_file_location("check_fail_open_mutation_under_test", SCRIPT)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


MOD = _load()


def _git(cwd, *args):
    return subprocess.run(["git", *args], cwd=str(cwd), check=True,
                          stdout=subprocess.PIPE, text=True).stdout


class DeclaredTestsStatic(unittest.TestCase):
    def test_b1_every_scenario_declares_valid_tests(self):
        for sc in MOD.SCENARIOS:
            with self.subTest(crate=sc.crate):
                tests = getattr(sc, "tests", None)
                self.assertIsNotNone(tests, "Scenario for %s has no `tests` attribute" % sc.crate)
                tests = list(tests)
                self.assertTrue(tests, "`tests` must be non-empty for %s" % sc.crate)
                self.assertTrue(all(isinstance(t, str) for t in tests), tests)
                i = 0
                while i < len(tests):
                    if tests[i] == "--lib":
                        i += 1
                    elif tests[i] == "--test":
                        self.assertLess(i + 1, len(tests), "dangling --test in %r" % (tests,))
                        name = tests[i + 1]
                        self.assertFalse(name.startswith("-"), tests)
                        f = REPO / "crates" / sc.crate / "tests" / (name + ".rs")
                        self.assertTrue(f.is_file(), "%s does not exist (from %r)" % (f, tests))
                        i += 2
                    elif tests[i] == "--bin":
                        # Added by the implementer (2026-11-14): the original
                        # spec omitted `--bin`, but specguard has no tests/ dir
                        # and only its binary's unit tests catch its mutation.
                        # Only the crate's own default binary (src/main.rs,
                        # bin name == crate name) is accepted.
                        self.assertLess(i + 1, len(tests), "dangling --bin in %r" % (tests,))
                        name = tests[i + 1]
                        self.assertEqual(name, sc.crate, tests)
                        f = REPO / "crates" / sc.crate / "src" / "main.rs"
                        self.assertTrue(f.is_file(), "%s does not exist (from %r)" % (f, tests))
                        i += 2
                    else:
                        self.fail("selector %r in %r is neither --lib, --test <name> nor --bin <crate>"
                                  % (tests[i], tests))


class DeclaredTestsEndToEnd(unittest.TestCase):
    CRATE = "blastguard"

    def setUp(self):
        self.scenario = next(s for s in MOD.SCENARIOS if s.crate == self.CRATE)
        self.tmp = Path(tempfile.mkdtemp(prefix="fom-declared-")).resolve()
        self.addCleanup(shutil.rmtree, self.tmp, ignore_errors=True)
        self.repo = self.tmp / "repo"
        (self.repo / "scripts").mkdir(parents=True)
        shutil.copy2(SCRIPT, self.repo / "scripts" / SCRIPT.name)
        target = self.repo / self.scenario.file_rel
        target.parent.mkdir(parents=True)
        target.write_text("// pre\n" + self.scenario.old + "// post\n", encoding="utf-8")
        _git(self.repo, "init", "-q")
        _git(self.repo, "config", "user.email", "t@example.invalid")
        _git(self.repo, "config", "user.name", "t")
        _git(self.repo, "add", "-A")
        _git(self.repo, "commit", "-q", "-m", "init")
        self.bin = self.tmp / "bin"
        self.bin.mkdir()
        self.log = self.tmp / "cargo-argv.txt"

    def _stub(self, body):
        stub = self.bin / "cargo"
        stub.write_text(
            "#!/bin/sh\n"
            f'_l=""; for a in "$@"; do _l="$_l\t$a"; done; printf \'%s\\n\' "$_l" >> "{self.log}"\n'
            + body,
            encoding="utf-8")
        stub.chmod(0o755)

    def _run(self):
        env = dict(os.environ, PATH=f"{self.bin}{os.pathsep}{os.environ['PATH']}")
        return subprocess.run(
            [sys.executable, str(self.repo / "scripts" / SCRIPT.name),
             "--crate", self.CRATE, "--keep-going"],
            cwd=str(self.repo), env=env, stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT, text=True, timeout=120)

    def _argvs(self):
        if not self.log.is_file():
            return []
        return [ln.split("\t")[1:] for ln in self.log.read_text().splitlines()]

    def test_b2_cargo_sees_exactly_the_declared_selectors(self):
        self._stub('echo "test result: FAILED. 0 passed; 1 failed"\nexit 101\n')
        p = self._run()
        declared = getattr(self.scenario, "tests", None)
        self.assertIsNotNone(declared, "Scenario has no `tests` attribute\n" + p.stdout)
        declared = list(declared)
        runs = [a for a in self._argvs() if a[:1] == ["test"]]
        self.assertEqual(len(runs), 1, "expected exactly one cargo test run: %r\n%s" % (runs, p.stdout))
        argv = runs[0]
        self.assertEqual(argv[:3], ["test", "-p", self.CRATE], argv)
        rest = argv[3:]
        n = len(declared)
        self.assertEqual(rest[:n], declared, "selectors seen by cargo: %r, declared: %r" % (rest, declared))
        tail = rest[n:]
        self.assertTrue(tail == [] or tail[0] == "--", "unexpected extra args after selectors: %r" % tail)
        self.assertEqual(p.returncode, 0, p.stdout)  # caught => script exits 0

    def test_b3_guard_passing_tests_are_not_caught(self):
        self._stub('echo "test result: ok. 3 passed; 0 failed"\nexit 0\n')
        p = self._run()
        self.assertNotEqual(p.returncode, 0, "cargo exit 0 must be NOT CAUGHT\n" + p.stdout)
        self.assertTrue(any(a[:1] == ["test"] for a in self._argvs()), "cargo test never ran\n" + p.stdout)

    def test_b4_guard_rejected_selector_is_not_a_caught_red(self):
        self._stub('echo "error: no test target named \\`x\\`" >&2\nexit 101\n')
        p = self._run()
        self.assertNotEqual(p.returncode, 0, "a cargo-rejected selector must not count as RED\n" + p.stdout)
        self.assertTrue(any(a[:1] == ["test"] for a in self._argvs()), "cargo test never ran\n" + p.stdout)


if __name__ == "__main__":
    unittest.main()
