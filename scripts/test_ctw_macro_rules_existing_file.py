#!/usr/bin/env python3
"""Regression: scripts/check-test-weakening.py stays UNDETERMINED (exit 2) for a
`macro_rules!` test template in a file that EXISTS at the base (GH #1525 fix).

The #1525 fix lets a brand-new file whose tests come from a `macro_rules!`
template (`#[test] fn $name()`) be judged clean, because with no base there is
nothing that could have been removed. For a file that exists at the base the
scanner cannot count macro invocations, so it must refuse to judge:

  (1) dropping one `t!(case, ...)` invocation removes a generated test with no
      textual `#[test]` / assertion change;
  (2) deleting the file removes every generated test;
  (3) rewriting explicit tests as a template collapses N tests into one
      template body.

Each must be exit 2 (Undetermined), never exit 0 (clean). The Undetermined
reason must name the macro_rules! template, so an unrelated exit-2 path (for
example the pre-fix "cannot attribute a #[test]" error) does not satisfy it.

Mutation check (recorded in the commit that added this file): deleting the
`if before_src and (before["template_tests"] or after["template_tests"])`
branch in scan() makes all three cases fail.
"""

import importlib.util
import unittest
from pathlib import Path

_HERE = Path(__file__).resolve().parent
_spec = importlib.util.spec_from_file_location(
    "ctw_tests", _HERE / "test_check_test_weakening.py"
)
_ctw = importlib.util.module_from_spec(_spec)
exec(  # noqa: S102 -- source-text load, see test_check_fail_open.py (05726f9f)
    compile(_spec.loader.get_source(_spec.name), _spec.origin, "exec", dont_inherit=True),
    _ctw.__dict__,
)

PATH = "crates/demo/tests/macro_tests.rs"

MACRO_TESTS_RS = """\
fn denied(cmd: &str) -> bool {
    !cmd.is_empty()
}

macro_rules! denies {
    ($name:ident, $cmd:expr) => {
        #[test]
        fn $name() {
            assert!(denied($cmd), "{} must be denied", $cmd);
        }
    };
}

denies!(denies_dollar, "echo $HOME");
denies!(denies_glob, "ls *");
"""

MACRO_TESTS_ONE_INVOCATION_RS = """\
fn denied(cmd: &str) -> bool {
    !cmd.is_empty()
}

macro_rules! denies {
    ($name:ident, $cmd:expr) => {
        #[test]
        fn $name() {
            assert!(denied($cmd), "{} must be denied", $cmd);
        }
    };
}

denies!(denies_dollar, "echo $HOME");
"""

EXPLICIT_TESTS_RS = """\
fn denied(cmd: &str) -> bool {
    !cmd.is_empty()
}

#[test]
fn denies_dollar() {
    assert!(denied("echo $HOME"), "must be denied");
}

#[test]
fn denies_glob() {
    assert!(denied("ls *"), "must be denied");
}
"""


class MacroRulesExistingFile(_ctw.GateTestCase):
    def assertUndeterminedTemplate(self, rc, out, err):
        self.assertEqual(
            rc, 2, f"expected undetermined (exit 2)\nSTDOUT:{out}\nSTDERR:{err}"
        )
        self.assertIn(
            "macro_rules! template",
            out + err,
            "the Undetermined reason must name the macro_rules! template",
        )

    def test_removed_macro_invocation_is_undetermined(self):
        repo = self.make_repo({PATH: MACRO_TESTS_RS})
        repo.write(PATH, MACRO_TESTS_ONE_INVOCATION_RS)
        repo.commit("test: drop one macro invocation")
        self.assertUndeterminedTemplate(*self.run_gate(repo.root))

    def test_deleted_macro_test_file_is_undetermined(self):
        repo = self.make_repo({PATH: MACRO_TESTS_RS})
        repo.remove(PATH)
        repo.commit("test: delete the macro test file")
        self.assertUndeterminedTemplate(*self.run_gate(repo.root))

    def test_explicit_tests_rewritten_as_template_is_undetermined(self):
        repo = self.make_repo({PATH: EXPLICIT_TESTS_RS})
        repo.write(PATH, MACRO_TESTS_ONE_INVOCATION_RS)
        repo.commit("test: rewrite explicit tests as a macro template")
        self.assertUndeterminedTemplate(*self.run_gate(repo.root))


if __name__ == "__main__":
    unittest.main()
