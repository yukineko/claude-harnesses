#!/usr/bin/env python3
"""Repro: scripts/check-test-weakening.py cannot attribute a `#[test]` that sits
inside a `macro_rules!` template, and answers UNDETERMINED (exit 2) for a
commit that only ADDS a new integration-test file.

Observed 2026-11-04 on the fde rulings branch: a new file
`crates/fde/tests/gone_target_unresolvable.rs` generated tests with

    macro_rules! gone_denies {
        ($name:ident, $cmd:expr) => {
            #[test]
            fn $name() { ... assert!(...); }
        };
    }

and the pre-commit gate blocked with
"cannot attribute a #[test] to a function body (unexpected text before `fn`:
'fn $name() {...')". The text between `#[test]` and the NEXT matched `fn NAME`
is the template `fn $name() {` (FN_DECL_RE does not match `$name`), so the
attribution fails.

Observable claim: a commit that adds a brand-new test file whose tests are
produced by a `macro_rules!` template (nothing removed, nothing weakened) is
judged clean (exit 0). At the measured rev the gate exits 2. Open: no
expectedFailure marker, so this file is RED until the scanner is fixed (the
fix may attribute the template body to the macro, or treat `fn $ident` as a
template function).

Control: the same tests written as explicit `#[test] fn` functions are clean,
so the failure is specific to the macro template.
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


class MacroRulesTestFn(_ctw.GateTestCase):
    def test_new_file_with_macro_rules_tests_is_clean(self):
        repo = self.make_repo()
        repo.write("crates/demo/tests/macro_tests.rs", MACRO_TESTS_RS)
        repo.commit("test: add a new test file whose tests come from macro_rules")
        self.assertClean(*self.run_gate(repo.root))

    def test_control_explicit_tests_are_clean(self):
        repo = self.make_repo()
        repo.write("crates/demo/tests/explicit_tests.rs", EXPLICIT_TESTS_RS)
        repo.commit("test: add a new test file with explicit test fns")
        self.assertClean(*self.run_gate(repo.root))


if __name__ == "__main__":
    unittest.main()
