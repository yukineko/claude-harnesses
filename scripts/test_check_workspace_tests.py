#!/usr/bin/env python3
"""Unit tests for scripts/check-workspace-tests.py.

Stdlib-only (`unittest`), no network, and — the point of the whole design —
NEVER a real `cargo test` and never a real workspace suite. Almost everything
below is either a pure function over captured text or `main()` driven with an
injected fake runner.

The ONE exception is the `RunProcess` class, and it is declared rather than
buried: `run_process`'s three stated invariants (stdin=/dev/null, stderr merged
into stdout, deadline kills the process GROUP) are not observable from a pure
function, and leaving the riskiest function on zero coverage to keep a "no
subprocess" boast would be the worse trade. Those cases launch throwaway
`sys.executable -c` one-liners — never cargo, never a repo suite. They cost
about 2.5s of the ~3s total; the other ~70 cases run in well under 0.1s.

Written by an agent that did NOT write the implementation (CLAUDE.md 2(a)) and
run against the un-implemented skeleton first, so every behavioural case here
was OBSERVED RED before any body existed (CLAUDE.md 2(b)).

Two structural notes about how the script is driven from here:

* `main(argv, runner, repo)` injects the runner and the repo, but it has NO
  parameter for the two RESOLVERS. So the cases that need "cargo is not
  installed" / "python3 is not installed" rebind `cws.resolve_cargo` and
  `cws.resolve_python` as module attributes and restore them afterwards — the
  same technique scripts/test_check_plugin_rollout.py uses for
  `SOURCE_CHANGED_SINCE`. That requires `main()` to reach them through the
  module global, not through a local alias or a `from ... import`. Touching the
  real PATH to get the same effect is not an option: it would make the verdict
  depend on the machine.
* Message wording is the implementer's, so the assertions below check the
  distinguishing FACTS a reader needs (a name, a number, a signal) plus one
  wording-agnostic property — every undetermined class must produce a message
  distinct from every other, or the classes are not distinguishable downstream
  at all.
"""
import ast
import importlib.util
import io
import os
import re
import sys
import tempfile
import time
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path

_HERE = Path(__file__).resolve().parent
_SRC_PATH = _HERE / "check-workspace-tests.py"
_SPEC = importlib.util.spec_from_file_location("check_workspace_tests", _SRC_PATH)
cws = importlib.util.module_from_spec(_SPEC)
# Compiled from the SOURCE TEXT on every import, deliberately NOT via
# `_SPEC.loader.exec_module(cws)`. SourceFileLoader consults __pycache__ and
# validates the cached .pyc on (source mtime, source size) — and mtime has
# ONE-SECOND granularity. So an edit that preserves the file's byte length and
# lands inside the same wall-clock second as the last compile is INVISIBLE:
# the interpreter serves the old bytecode and this suite tests code that is no
# longer on disk. Measured 2026-09-10: a mutation-kill sweep reported the
# `main()` ranking mutant (a pure reorder — identical byte length) as a
# SURVIVOR for exactly this reason. It was not; the mutation had never been
# loaded. A stale cache can only ever manufacture a false GREEN, never a false
# red, which is what makes it worth removing rather than working around.
# Do not "simplify" this back to exec_module.
exec(  # noqa: S102 - see above; the cache path is the defect being avoided
    compile(_SRC_PATH.read_text(encoding="utf-8"), str(_SRC_PATH), "exec"),
    cws.__dict__,
)


# --------------------------------------------------------------------------
# Fixture text. Shaped like real cargo/libtest and real unittest output, because
# a parser tested only against text the test author invented is a parser tested
# against nothing.
# --------------------------------------------------------------------------

CARGO_MIXED = """\
   Compiling blastguard v1.2.0 (/repo/crates/blastguard)
    Finished `test` profile [unoptimized + debuginfo] target(s) in 12.34s
     Running unittests src/lib.rs (target/debug/deps/blastguard-9a2f1c11e0d4b7aa)
test detect::tests::bg1_allows_plain_ls ... ok
test detect::tests::bg2_x ... FAILED
test result: FAILED. 318 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.20s
     Running tests/scoped_destructive.rs (target/debug/deps/scoped_destructive-1122ee33445566aa)
test scoped::guards_a_scoped_rm ... ok
test result: ok. 20 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.42s
   Doc-tests blastguard
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
error: test failed, to rerun pass `-p blastguard --lib`
"""

CARGO_GREEN = """\
    Finished `test` profile [unoptimized + debuginfo] target(s) in 3.10s
     Running unittests src/lib.rs (target/debug/deps/harness_core-aabbccdd11223344)
test verdict::tests::clean_is_unforgeable ... ok
test result: ok. 42 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.31s
   Doc-tests harness-core
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.10s
"""

# A named red in a single target, with the trailing `failures:` recap libtest
# always prints. reported_failed (1) must equal len(failed_names) (1): if the
# recap block were also scraped as failing names the count check would fire and
# a REAL, nameable red would be misreported as undetermined.
CARGO_ONE_RED = """\
     Running unittests src/lib.rs (target/debug/deps/blastguard-9a2f1c11e0d4b7aa)
test detect::tests::bg1_allows_plain_ls ... ok
test detect::tests::bg2_x ... FAILED

failures:

---- detect::tests::bg2_x stdout ----
thread 'detect::tests::bg2_x' panicked at crates/blastguard/src/detect.rs:88:9:
assertion `left == right` failed

failures:
    detect::tests::bg2_x

test result: FAILED. 318 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.20s
error: test failed, to rerun pass `-p blastguard --lib`
"""

# What a build/link error looks like: cargo exits non-zero and NO `test result:`
# line is ever printed, so "0 failures" would be a lie about a body that never
# ran a single test.
CARGO_BUILD_ERROR = """\
   Compiling blastguard v1.2.0 (/repo/crates/blastguard)
error[E0425]: cannot find value `verdict` in this scope
  --> crates/blastguard/src/detect.rs:12:9
error: could not compile `blastguard` (lib test) due to 1 previous error
"""

UNITTEST_OK = """\
....................
----------------------------------------------------------------------
Ran 120 tests in 67.6s

OK
"""

UNITTEST_OK_SKIPPED = """\
...s..s.............
----------------------------------------------------------------------
Ran 120 tests in 6.6s

OK (skipped=2)
"""

UNITTEST_FAILED = """\
..FF..E.............
======================================================================
FAIL: test_gate_crate_absent (scripts.test_check_plugin_rollout.Enablement.test_gate_crate_absent)
----------------------------------------------------------------------
Traceback (most recent call last):
  File "/repo/scripts/test_check_plugin_rollout.py", line 402, in test_gate_crate_absent
    self.assertEqual(rc, cpr.RC_ENABLEMENT)
AssertionError: 0 != 3
======================================================================
FAIL: test_explicitly_false (scripts.test_check_plugin_rollout.Enablement.test_explicitly_false)
----------------------------------------------------------------------
AssertionError: 0 != 3
======================================================================
ERROR: test_absent_registry (scripts.test_check_plugin_rollout.RolloutDrift.test_absent_registry)
----------------------------------------------------------------------
KeyError: 'plugins'
----------------------------------------------------------------------
Ran 42 tests in 1.9s

FAILED (failures=2, errors=1)
"""

# unittest's synthetic shape when a module cannot be imported at all. The suite
# did not run; this is not "one test failed".
UNITTEST_IMPORT_ERROR = """\
E
======================================================================
ERROR: test_check_workspace_tests (unittest.loader._FailedTest.test_check_workspace_tests)
----------------------------------------------------------------------
ImportError: Failed to import test module: scripts.test_check_workspace_tests
Traceback (most recent call last):
  File "/opt/python3.12/unittest/loader.py", line 407, in _find_test_path
    module = self._get_module_from_name(name)
ModuleNotFoundError: No module named 'yaml'

----------------------------------------------------------------------
Ran 1 test in 0.001s

FAILED (errors=1)
"""

UNITTEST_UNPARSEABLE = """\
zsh: killed     python3 -m unittest scripts.test_x
"""


def _completed(argv=("cargo", "test"), rc=0, out="", launch_error=None,
               timed_out=False, deadline=1800.0):
    return cws.Completed(
        argv=tuple(argv),
        rc=rc,
        out=out,
        launch_error=launch_error,
        timed_out=timed_out,
        deadline=deadline,
    )


def _cargo_parse(targets=(), failed_names=(), reported_failed=0, result_lines=0):
    return cws.CargoParse(
        targets=tuple(targets),
        failed_names=tuple(failed_names),
        reported_failed=reported_failed,
        result_lines=result_lines,
    )


def _unittest_parse(ran=None, outcome=None, failed_names=(), import_error=False):
    return cws.UnittestParse(
        ran=ran,
        outcome=outcome,
        failed_names=tuple(failed_names),
        import_error=import_error,
    )


class _ReportAsserts(unittest.TestCase):
    """Shared vocabulary for "which list did this land in, and what did it say"."""

    def messages(self, report):
        out = []
        for bucket in (report.failures, report.undetermined, report.deadline):
            for item in bucket:
                out.append(item if isinstance(item, str) else str(item))
        return out

    def joined(self, report):
        return "\n".join(self.messages(report))

    def assert_only(self, report, bucket_name):
        """Exactly `bucket_name` is non-empty; the other two verdict lists are empty."""
        buckets = {
            "failures": report.failures,
            "undetermined": report.undetermined,
            "deadline": report.deadline,
        }
        self.assertTrue(
            buckets[bucket_name],
            f"expected a finding in `{bucket_name}`, got report={report!r}",
        )
        for name, value in buckets.items():
            if name != bucket_name:
                self.assertFalse(
                    value,
                    f"finding landed in `{name}` as well as `{bucket_name}`: {report!r}",
                )

    def assert_not_clean(self, report):
        self.assertTrue(
            report.failures or report.undetermined or report.deadline,
            f"report reads as clean: {report!r}",
        )

    def assert_clean(self, report):
        self.assertEqual(tuple(report.failures), ())
        self.assertEqual(tuple(report.undetermined), ())
        self.assertEqual(tuple(report.deadline), ())

    def assert_mentions_any(self, text, needles, why):
        low = text.lower()
        self.assertTrue(
            any(n.lower() in low for n in needles),
            f"{why}\nnone of {list(needles)} appear in: {text!r}",
        )


# ==========================================================================
# parse_cargo_test_output
# ==========================================================================

class ParseCargo(unittest.TestCase):
    def test_targets_are_captured_in_announcement_order(self):
        p = cws.parse_cargo_test_output(CARGO_MIXED)
        self.assertEqual(
            len(p.targets), 3,
            f"two Running banners plus one Doc-tests banner, got {p.targets!r}",
        )
        # Pinned verbatim: this exact label is the example the contract gives
        # for the format ("blastguard (unittests src/lib.rs)").
        self.assertEqual(p.targets[0], "blastguard (unittests src/lib.rs)")
        self.assertIn("scoped_destructive", p.targets[1])
        self.assertIn("tests/scoped_destructive.rs", p.targets[1])
        self.assertIn("blastguard", p.targets[2])
        self.assertIn("doc-test", p.targets[2].lower())

    def test_failing_name_is_attributed_to_the_preceding_target(self):
        p = cws.parse_cargo_test_output(CARGO_MIXED)
        self.assertEqual(
            p.failed_names,
            (("blastguard (unittests src/lib.rs)", "detect::tests::bg2_x"),),
        )

    def test_failure_before_any_banner_is_unknown_target(self):
        """Attributing it to whatever target is handy would invent a fact."""
        text = (
            "test early::orphan ... FAILED\n"
            "     Running unittests src/lib.rs "
            "(target/debug/deps/blastguard-9a2f1c11e0d4b7aa)\n"
            "test detect::tests::bg2_x ... FAILED\n"
            "test result: FAILED. 1 passed; 2 failed; 0 ignored; 0 measured; "
            "0 filtered out; finished in 0.1s\n"
        )
        p = cws.parse_cargo_test_output(text)
        self.assertEqual(len(p.failed_names), 2, p.failed_names)
        self.assertEqual(p.failed_names[0][0], "<unknown target>")
        self.assertEqual(p.failed_names[0][1], "early::orphan")
        self.assertNotEqual(
            p.failed_names[1][0], "<unknown target>",
            "the SECOND failure does have a banner before it and must be "
            "attributed to it",
        )

    def test_reported_failed_sums_every_result_line(self):
        text = (
            "     Running unittests src/lib.rs (target/debug/deps/a-1111)\n"
            "test result: FAILED. 10 passed; 2 failed; 0 ignored; 0 measured; "
            "0 filtered out; finished in 0.1s\n"
            "     Running unittests src/lib.rs (target/debug/deps/b-2222)\n"
            "test result: FAILED. 4 passed; 3 failed; 0 ignored; 0 measured; "
            "0 filtered out; finished in 0.1s\n"
            "     Running unittests src/lib.rs (target/debug/deps/c-3333)\n"
            "test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; "
            "0 filtered out; finished in 0.1s\n"
        )
        p = cws.parse_cargo_test_output(text)
        self.assertEqual(p.reported_failed, 5)
        self.assertEqual(p.result_lines, 3)

    def test_result_lines_counted_on_mixed_text(self):
        p = cws.parse_cargo_test_output(CARGO_MIXED)
        self.assertEqual(p.result_lines, 3)
        self.assertEqual(p.reported_failed, 1)

    def test_zero_result_lines_is_representable(self):
        """A build error prints no `test result:` at all; that must be a 0 the
        verdict can see, not an absence that reads as a green."""
        p = cws.parse_cargo_test_output(CARGO_BUILD_ERROR)
        self.assertEqual(p.result_lines, 0)
        self.assertEqual(p.reported_failed, 0)
        self.assertEqual(p.failed_names, ())

    def test_all_green_text_has_no_failures(self):
        p = cws.parse_cargo_test_output(CARGO_GREEN)
        self.assertEqual(p.failed_names, ())
        self.assertEqual(p.reported_failed, 0)
        self.assertGreater(p.result_lines, 0)

    def test_failures_recap_block_is_not_double_counted(self):
        """libtest re-lists every failing name under `failures:`. Counting those
        would make reported_failed != len(failed_names) and turn a nameable red
        into an undetermined."""
        p = cws.parse_cargo_test_output(CARGO_ONE_RED)
        self.assertEqual(p.reported_failed, 1)
        self.assertEqual(len(p.failed_names), 1, p.failed_names)
        self.assertEqual(p.failed_names[0][1], "detect::tests::bg2_x")

    def test_empty_text_is_all_zeroes_not_a_crash(self):
        p = cws.parse_cargo_test_output("")
        self.assertEqual(p.targets, ())
        self.assertEqual(p.failed_names, ())
        self.assertEqual(p.reported_failed, 0)
        self.assertEqual(p.result_lines, 0)


# ==========================================================================
# cargo_verdict
# ==========================================================================

class CargoVerdict(_ReportAsserts):
    def test_timed_out_goes_to_deadline_and_names_limit_and_target(self):
        parse = cws.parse_cargo_test_output(CARGO_MIXED)
        c = _completed(rc=-9, out=CARGO_MIXED, timed_out=True, deadline=1800.0)
        r = cws.cargo_verdict(c, parse)
        self.assert_only(r, "deadline")
        msg = self.joined(r)
        self.assertIn("1800", msg, "the deadline number must be in the message")
        self.assertIn(
            "blastguard", msg,
            "the last announced target is where the reader has to look",
        )

    def test_launch_error_is_undetermined(self):
        c = _completed(rc=0, out="", launch_error="[Errno 2] No such file: 'cargo'")
        r = cws.cargo_verdict(c, _cargo_parse())
        self.assert_only(r, "undetermined")
        self.assertIn("cargo", self.joined(r).lower())

    def test_killed_by_signal_is_undetermined_and_names_the_signal(self):
        c = _completed(rc=-9, out=CARGO_GREEN)
        r = cws.cargo_verdict(c, cws.parse_cargo_test_output(CARGO_GREEN))
        self.assert_only(r, "undetermined")
        msg = self.joined(r)
        self.assertIn("9", msg, "the signal number must be named")
        self.assert_mentions_any(msg, ["signal", "SIG"], "the class must be readable")

    def test_nonzero_rc_with_no_result_lines_never_ran_tests(self):
        """The build-error shape. Reporting "0 failures" here is the exact lie
        this gate exists to stop."""
        parse = cws.parse_cargo_test_output(CARGO_BUILD_ERROR)
        c = _completed(rc=101, out=CARGO_BUILD_ERROR)
        r = cws.cargo_verdict(c, parse)
        self.assert_only(r, "undetermined")
        msg = self.joined(r)
        self.assert_mentions_any(
            msg,
            ["never", "no test result", "no `test result", "did not run",
             "build", "link"],
            "the message must say the tests were never reached",
        )
        self.assertNotIn("0 failures", msg)
        self.assertNotIn("0 failing", msg)

    def test_rc_zero_but_text_reports_failures_is_a_contradiction(self):
        parse = _cargo_parse(
            targets=("blastguard (unittests src/lib.rs)",),
            failed_names=(("blastguard (unittests src/lib.rs)", "a::b"),),
            reported_failed=1,
            result_lines=1,
        )
        r = cws.cargo_verdict(_completed(rc=0, out=CARGO_ONE_RED), parse)
        self.assert_only(r, "undetermined")

    def test_rc_nonzero_but_text_reports_no_failures_is_a_contradiction(self):
        parse = _cargo_parse(
            targets=("harness_core (unittests src/lib.rs)",),
            reported_failed=0,
            result_lines=2,
        )
        r = cws.cargo_verdict(_completed(rc=101, out=CARGO_GREEN), parse)
        self.assert_only(r, "undetermined")

    def test_count_and_names_disagreeing_is_undetermined(self):
        """If three failed but only one name is extractable, printing the one
        name would be an incomplete report presented as complete."""
        parse = _cargo_parse(
            targets=("blastguard (unittests src/lib.rs)",),
            failed_names=(("blastguard (unittests src/lib.rs)", "a::b"),),
            reported_failed=3,
            result_lines=1,
        )
        r = cws.cargo_verdict(_completed(rc=101, out=CARGO_ONE_RED), parse)
        self.assert_only(r, "undetermined")
        self.assert_mentions_any(
            self.joined(r), ["3", "count", "name"],
            "the mismatch itself must be legible",
        )

    def test_named_failure_with_matching_count_is_a_failure(self):
        """The rc=1 path: a real, nameable red goes to `failures` — not to
        undetermined, or exit 3 would swallow every genuine cargo failure."""
        parse = cws.parse_cargo_test_output(CARGO_ONE_RED)
        r = cws.cargo_verdict(_completed(rc=101, out=CARGO_ONE_RED), parse)
        self.assert_only(r, "failures")
        msg = self.joined(r)
        self.assertIn("detect::tests::bg2_x", msg)
        self.assertIn("blastguard", msg)

    def test_clean_run_is_clean(self):
        parse = cws.parse_cargo_test_output(CARGO_GREEN)
        r = cws.cargo_verdict(_completed(rc=0, out=CARGO_GREEN), parse)
        self.assert_clean(r)

    def test_every_undetermined_class_has_a_distinct_message(self):
        """Wording is the implementer's, but two classes that print the SAME
        string are not distinguishable, and the whole reason these are separate
        branches is that their remedies differ."""
        cases = {
            "launch_error": (
                _completed(rc=0, launch_error="No such file"), _cargo_parse()
            ),
            "signal": (
                _completed(rc=-9, out=CARGO_GREEN),
                cws.parse_cargo_test_output(CARGO_GREEN),
            ),
            "never_ran": (
                _completed(rc=101, out=CARGO_BUILD_ERROR),
                cws.parse_cargo_test_output(CARGO_BUILD_ERROR),
            ),
            "rc0_with_failures": (
                _completed(rc=0, out=CARGO_ONE_RED),
                cws.parse_cargo_test_output(CARGO_ONE_RED),
            ),
            "rc_nonzero_no_failures": (
                _completed(rc=101, out=CARGO_GREEN),
                cws.parse_cargo_test_output(CARGO_GREEN),
            ),
            "count_mismatch": (
                _completed(rc=101, out=CARGO_ONE_RED),
                _cargo_parse(
                    targets=("blastguard (unittests src/lib.rs)",),
                    failed_names=(("blastguard (unittests src/lib.rs)", "a::b"),),
                    reported_failed=3,
                    result_lines=1,
                ),
            ),
        }
        seen = {}
        for name, (completed, parse) in cases.items():
            r = cws.cargo_verdict(completed, parse)
            self.assert_not_clean(r)
            msg = self.joined(r)
            clash = seen.get(msg)
            self.assertIsNone(
                clash,
                f"class `{name}` and class `{clash}` produce the identical "
                f"message {msg!r}",
            )
            seen[msg] = name


# ==========================================================================
# discover_python_suites
# ==========================================================================

class DiscoverPythonSuites(unittest.TestCase):
    def _scripts_dir(self, tmp, names):
        d = Path(tmp) / "scripts"
        d.mkdir(parents=True, exist_ok=True)
        for n in names:
            (d / n).write_text("# fixture\n", encoding="utf-8")
        return str(d)

    def test_finds_test_modules_and_ignores_everything_else(self):
        with tempfile.TemporaryDirectory() as tmp:
            d = self._scripts_dir(
                tmp, ["test_a.py", "test_b.py", "helper.py", "notes.md",
                      "check-thing.py"]
            )
            modules, problem = cws.discover_python_suites(d)
        self.assertIsNone(problem, f"clean dir must have no problem: {problem!r}")
        self.assertEqual(tuple(modules), ("scripts.test_a", "scripts.test_b"))

    def test_directory_with_no_suites_is_a_problem_not_an_empty_success(self):
        """CLAUDE.md 3: never return an empty collection on error. This repo has
        23 suites; zero means the glob or the checkout is broken."""
        with tempfile.TemporaryDirectory() as tmp:
            d = self._scripts_dir(tmp, ["helper.py", "README.md"])
            modules, problem = cws.discover_python_suites(d)
        self.assertEqual(tuple(modules), ())
        self.assertIsInstance(problem, str)
        self.assertTrue(problem.strip(), "the problem string must say something")

    def test_empty_result_cannot_be_mistaken_for_success(self):
        """The caller-side half of the same rule: `modules` alone is ambiguous
        (empty == "nothing to run" == "everything passed"), so the ONLY thing
        separating the two states is `problem` being non-None. Pinned as its own
        case because a return of `((), None)` would satisfy the test above's
        `modules == ()` assertion while re-opening the exact fail-open."""
        with tempfile.TemporaryDirectory() as tmp:
            d = self._scripts_dir(tmp, [])
            modules, problem = cws.discover_python_suites(d)
        self.assertFalse(modules)
        self.assertIsNotNone(
            problem,
            "an empty `modules` with problem=None is indistinguishable from a "
            "clean run of zero suites",
        )

    def test_nonexistent_directory_is_a_problem(self):
        with tempfile.TemporaryDirectory() as tmp:
            missing = str(Path(tmp) / "no-such-dir")
            modules, problem = cws.discover_python_suites(missing)
        self.assertEqual(tuple(modules), ())
        self.assertIsInstance(problem, str)
        self.assertTrue(problem.strip())

    def test_a_file_where_the_directory_should_be_is_a_problem(self):
        """An unreadable/wrong-type path must not degrade to "no suites"."""
        with tempfile.TemporaryDirectory() as tmp:
            f = Path(tmp) / "scripts"
            f.write_text("not a directory\n", encoding="utf-8")
            modules, problem = cws.discover_python_suites(str(f))
        self.assertEqual(tuple(modules), ())
        self.assertIsInstance(problem, str)


# ==========================================================================
# parse_unittest_output
# ==========================================================================

class ParseUnittest(unittest.TestCase):
    def test_ok_run(self):
        p = cws.parse_unittest_output(UNITTEST_OK)
        self.assertEqual(p.ran, 120)
        self.assertEqual(p.outcome, "OK")
        self.assertEqual(tuple(p.failed_names), ())
        self.assertFalse(p.import_error)

    def test_ok_with_skips_is_still_ok(self):
        p = cws.parse_unittest_output(UNITTEST_OK_SKIPPED)
        self.assertEqual(p.ran, 120)
        self.assertEqual(p.outcome, "OK")
        self.assertEqual(tuple(p.failed_names), ())

    def test_failed_run_captures_every_name(self):
        p = cws.parse_unittest_output(UNITTEST_FAILED)
        self.assertEqual(p.ran, 42)
        self.assertEqual(p.outcome, "FAILED")
        self.assertFalse(p.import_error)
        self.assertEqual(len(p.failed_names), 3, p.failed_names)
        blob = " ".join(p.failed_names)
        for needle in ("test_gate_crate_absent", "test_explicitly_false",
                       "test_absent_registry"):
            self.assertIn(needle, blob)

    def test_import_failure_is_flagged_not_counted_as_a_failing_test(self):
        p = cws.parse_unittest_output(UNITTEST_IMPORT_ERROR)
        self.assertTrue(
            p.import_error,
            "unittest.loader._FailedTest means the module never imported",
        )
        self.assertEqual(p.outcome, "FAILED")

    def test_ordinary_failure_is_not_an_import_error(self):
        """Control arm: without this, `import_error = True` always would satisfy
        the case above and destroy the distinction."""
        p = cws.parse_unittest_output(UNITTEST_FAILED)
        self.assertFalse(p.import_error)

    def test_output_without_a_ran_line_is_unparseable(self):
        p = cws.parse_unittest_output(UNITTEST_UNPARSEABLE)
        self.assertIsNone(
            p.ran, "no `Ran N tests` line means the count is unknown, not zero"
        )

    def test_empty_output_is_unparseable(self):
        p = cws.parse_unittest_output("")
        self.assertIsNone(p.ran)
        self.assertIsNone(p.outcome)


# ==========================================================================
# python_suite_verdict
# ==========================================================================

MOD = "scripts.test_check_plugin_rollout"


class PythonSuiteVerdict(_ReportAsserts):
    def test_timed_out_goes_to_deadline(self):
        c = _completed(rc=-9, out="", timed_out=True, deadline=600.0)
        r = cws.python_suite_verdict(MOD, c, cws.parse_unittest_output(""))
        self.assert_only(r, "deadline")
        msg = self.joined(r)
        self.assertIn("600", msg)
        self.assertIn(MOD, msg)

    def test_launch_error_is_undetermined(self):
        c = _completed(rc=0, launch_error="[Errno 2] No such file: 'python3'")
        r = cws.python_suite_verdict(MOD, c, cws.parse_unittest_output(""))
        self.assert_only(r, "undetermined")
        self.assertIn(MOD, self.joined(r))

    def test_signal_is_undetermined_and_named(self):
        c = _completed(rc=-9, out=UNITTEST_OK)
        r = cws.python_suite_verdict(MOD, c, cws.parse_unittest_output(UNITTEST_OK))
        self.assert_only(r, "undetermined")
        msg = self.joined(r)
        self.assertIn("9", msg)
        self.assert_mentions_any(msg, ["signal", "SIG"], "the class must be readable")

    def test_import_error_says_the_suite_never_ran(self):
        """`FAILED (errors=1)` over an import failure would print as "1 test
        failed" — a suite of 40 tests that executed none of them."""
        p = cws.parse_unittest_output(UNITTEST_IMPORT_ERROR)
        r = cws.python_suite_verdict(MOD, _completed(rc=1, out=UNITTEST_IMPORT_ERROR), p)
        self.assert_only(r, "undetermined")
        msg = self.joined(r)
        self.assert_mentions_any(
            msg, ["import", "never ran", "did not run", "never executed"],
            "the message must name the import failure, not a failing test",
        )
        self.assertNotIn("1 test failed", msg)
        self.assertNotIn("1 failing test", msg)

    def test_unparseable_output_is_undetermined(self):
        p = cws.parse_unittest_output(UNITTEST_UNPARSEABLE)
        r = cws.python_suite_verdict(MOD, _completed(rc=1, out=UNITTEST_UNPARSEABLE), p)
        self.assert_only(r, "undetermined")

    def test_missing_outcome_is_undetermined(self):
        p = _unittest_parse(ran=12, outcome=None)
        r = cws.python_suite_verdict(MOD, _completed(rc=0, out="Ran 12 tests"), p)
        self.assert_only(r, "undetermined")

    def test_ran_zero_tests_proved_nothing(self):
        text = "\n----------------------------------------------------------------------\nRan 0 tests in 0.000s\n\nOK\n"
        p = cws.parse_unittest_output(text)
        self.assertEqual(p.ran, 0, "fixture sanity: the parse must see the 0")
        r = cws.python_suite_verdict(MOD, _completed(rc=0, out=text), p)
        self.assert_only(r, "undetermined")
        self.assert_mentions_any(
            self.joined(r), ["0 test", "no test", "zero test", "proved nothing"],
            "an exit-0 suite that ran nothing must say so",
        )

    def test_ok_with_nonzero_rc_is_a_contradiction(self):
        p = cws.parse_unittest_output(UNITTEST_OK)
        r = cws.python_suite_verdict(MOD, _completed(rc=1, out=UNITTEST_OK), p)
        self.assert_only(r, "undetermined")

    def test_failed_with_zero_rc_is_a_contradiction(self):
        p = cws.parse_unittest_output(UNITTEST_FAILED)
        r = cws.python_suite_verdict(MOD, _completed(rc=0, out=UNITTEST_FAILED), p)
        self.assert_only(r, "undetermined")

    def test_failed_without_extractable_names_is_undetermined(self):
        """Cannot name the red -> cannot report it, and "something failed" with
        no name is not a detection anyone can act on."""
        p = _unittest_parse(ran=42, outcome="FAILED", failed_names=())
        r = cws.python_suite_verdict(MOD, _completed(rc=1, out="..."), p)
        self.assert_only(r, "undetermined")

    def test_failed_with_names_is_a_failure_naming_module_and_tests(self):
        p = cws.parse_unittest_output(UNITTEST_FAILED)
        r = cws.python_suite_verdict(MOD, _completed(rc=1, out=UNITTEST_FAILED), p)
        self.assert_only(r, "failures")
        msg = self.joined(r)
        self.assertIn(MOD, msg)
        for needle in ("test_gate_crate_absent", "test_explicitly_false",
                       "test_absent_registry"):
            self.assertIn(needle, msg, "every failing name must be printed")

    def test_ok_rc_zero_positive_count_is_clean(self):
        p = cws.parse_unittest_output(UNITTEST_OK)
        r = cws.python_suite_verdict(MOD, _completed(rc=0, out=UNITTEST_OK), p)
        self.assert_clean(r)

    def test_every_python_undetermined_class_has_a_distinct_message(self):
        zero = "Ran 0 tests in 0.000s\n\nOK\n"
        cases = {
            "launch_error": (
                _completed(rc=0, launch_error="No such file"),
                cws.parse_unittest_output(""),
            ),
            "signal": (
                _completed(rc=-9, out=UNITTEST_OK),
                cws.parse_unittest_output(UNITTEST_OK),
            ),
            "import_error": (
                _completed(rc=1, out=UNITTEST_IMPORT_ERROR),
                cws.parse_unittest_output(UNITTEST_IMPORT_ERROR),
            ),
            "unparseable": (
                _completed(rc=1, out=UNITTEST_UNPARSEABLE),
                cws.parse_unittest_output(UNITTEST_UNPARSEABLE),
            ),
            "ran_zero": (
                _completed(rc=0, out=zero), cws.parse_unittest_output(zero)
            ),
            "ok_rc_nonzero": (
                _completed(rc=1, out=UNITTEST_OK),
                cws.parse_unittest_output(UNITTEST_OK),
            ),
            "failed_rc_zero": (
                _completed(rc=0, out=UNITTEST_FAILED),
                cws.parse_unittest_output(UNITTEST_FAILED),
            ),
            "failed_no_names": (
                _completed(rc=1, out="..."),
                _unittest_parse(ran=42, outcome="FAILED", failed_names=()),
            ),
        }
        seen = {}
        for name, (completed, parse) in cases.items():
            r = cws.python_suite_verdict(MOD, completed, parse)
            self.assert_not_clean(r)
            msg = self.joined(r)
            clash = seen.get(msg)
            self.assertIsNone(
                clash,
                f"class `{name}` and class `{clash}` produce the identical "
                f"message {msg!r}",
            )
            seen[msg] = name


# ==========================================================================
# resolve_cargo / resolve_python  (all lookups injected; the real PATH and the
# real $HOME are never consulted, or the verdict would depend on the machine)
# ==========================================================================

class ResolveCargo(unittest.TestCase):
    ENV = {"HOME": "/home/fixture"}

    def _resolve(self, on_path=None, env_file=False, bin_cargo=False):
        files = set()
        if env_file:
            files.add("/home/fixture/.cargo/env")
        if bin_cargo:
            files.add("/home/fixture/.cargo/bin/cargo")
        return cws.resolve_cargo(
            repo="/repo",
            environ=dict(self.ENV),
            which=lambda name: on_path,
            isfile=lambda p: str(p) in files,
        )

    def test_cargo_on_path_resolves(self):
        r = self._resolve(on_path="/usr/local/bin/cargo")
        self.assertEqual(r.path, "/usr/local/bin/cargo")
        self.assertIsNone(r.problem)

    def test_no_path_and_no_rustup_env_file_is_a_problem(self):
        r = self._resolve(on_path=None, env_file=False)
        self.assertIsNone(r.path)
        self.assertIsInstance(r.problem, str)
        self.assertTrue(r.problem.strip())

    def test_rustup_home_install_is_found(self):
        r = self._resolve(on_path=None, env_file=True, bin_cargo=True)
        self.assertIsNone(r.problem, r.problem)
        self.assertEqual(r.path, "/home/fixture/.cargo/bin/cargo")

    def test_env_file_without_bin_cargo_gets_its_own_message(self):
        """Different remedies (install rustup vs repair a broken toolchain
        directory) must not collapse into one string."""
        no_env = self._resolve(on_path=None, env_file=False)
        broken = self._resolve(on_path=None, env_file=True, bin_cargo=False)
        self.assertIsNone(broken.path)
        self.assertIsInstance(broken.problem, str)
        self.assertTrue(broken.problem.strip())
        self.assertNotEqual(
            broken.problem, no_env.problem,
            "'no rustup at all' and 'rustup env file but no bin/cargo' are "
            "different remedies and must read differently",
        )

    def test_never_both_none(self):
        """The Resolved contract: exactly one of path/problem is non-None."""
        for kwargs in (
            {"on_path": "/usr/bin/cargo"},
            {"on_path": None, "env_file": False},
            {"on_path": None, "env_file": True, "bin_cargo": True},
            {"on_path": None, "env_file": True, "bin_cargo": False},
        ):
            with self.subTest(**kwargs):
                r = self._resolve(**kwargs)
                self.assertNotEqual(
                    (r.path is None, r.problem is None), (True, True),
                    "both None makes 'cannot determine' equal to 'right here'",
                )
                self.assertNotEqual((r.path is None, r.problem is None), (False, False))


class ResolvePython(unittest.TestCase):
    def _with_executable(self, value, which):
        saved = sys.executable
        sys.executable = value
        try:
            return cws.resolve_python(which=which)
        finally:
            sys.executable = saved

    def test_sys_executable_is_preferred(self):
        r = self._with_executable("/opt/py/bin/python3", lambda n: "/usr/bin/python3")
        self.assertIsNone(r.problem, r.problem)
        self.assertEqual(r.path, "/opt/py/bin/python3")

    def test_path_is_the_fallback(self):
        r = self._with_executable("", lambda n: "/usr/bin/python3")
        self.assertIsNone(r.problem, r.problem)
        self.assertEqual(r.path, "/usr/bin/python3")

    def test_missing_everywhere_is_a_problem(self):
        r = self._with_executable("", lambda n: None)
        self.assertIsNone(r.path)
        self.assertIsInstance(r.problem, str)
        self.assertTrue(r.problem.strip())


# ==========================================================================
# main() end-to-end, with an injected fake runner. No real process is launched.
# ==========================================================================

class FakeRunner:
    """Stands in for run_process. Same signature: (argv, cwd, deadline, env=None).

    Keys off argv: a `-m unittest scripts.<mod>` invocation is a python suite
    (looked up in `suites`, defaulting to `default_suite`), anything else is the
    cargo body.
    """

    def __init__(self, cargo=None, suites=None, default_suite=None):
        self.calls = []
        self.cargo = cargo
        self.suites = dict(suites or {})
        self.default_suite = default_suite

    def module_of(self, argv):
        for a in argv:
            if isinstance(a, str) and a.startswith("scripts.test_"):
                return a
        return None

    def __call__(self, argv, cwd, deadline, env=None):
        argv = list(argv)
        self.calls.append((tuple(argv), cwd, deadline))
        mod = self.module_of(argv)
        if mod is not None or "unittest" in argv:
            canned = self.suites.get(mod, self.default_suite)
            if canned is None:
                canned = _completed(rc=0, out=UNITTEST_OK, deadline=deadline)
            return canned._replace(argv=tuple(argv), deadline=deadline)
        canned = self.cargo
        if canned is None:
            canned = _completed(rc=0, out=CARGO_GREEN, deadline=deadline)
        return canned._replace(argv=tuple(argv), deadline=deadline)

    @property
    def cargo_calls(self):
        return [c for c in self.calls if self.module_of(c[0]) is None]

    @property
    def python_calls(self):
        return [c for c in self.calls if self.module_of(c[0]) is not None]


def _fixture_repo(tmp, *, crates=True, scripts=True, suites=("test_alpha", "test_beta")):
    root = Path(tmp)
    if crates:
        (root / "crates" / "blastguard" / "src").mkdir(parents=True, exist_ok=True)
        (root / "crates" / "blastguard" / "src" / "lib.rs").write_text(
            "// fixture\n", encoding="utf-8"
        )
    if scripts:
        s = root / "scripts"
        s.mkdir(parents=True, exist_ok=True)
        (s / "helper.py").write_text("# not a suite\n", encoding="utf-8")
        for name in suites:
            (s / f"{name}.py").write_text("# fixture suite\n", encoding="utf-8")
    return str(root)


class MainCase(unittest.TestCase):
    """Drives main() with the resolvers stubbed at module level.

    `main(argv, runner, repo)` has no parameter for the resolvers, so they are
    rebound here and restored in `finally` — the same module-attribute technique
    scripts/test_check_plugin_rollout.py uses. Touching the real PATH instead
    would make these cases depend on the developer's machine.
    """

    CARGO_OK = cws.Resolved(path="/fixture/bin/cargo", problem=None)
    PY_OK = cws.Resolved(path="/fixture/bin/python3", problem=None)

    def run_main(self, repo, runner, *, argv=(), cargo=None, python=None):
        saved = (cws.resolve_cargo, cws.resolve_python)
        cws.resolve_cargo = lambda *a, **k: (cargo or self.CARGO_OK)
        cws.resolve_python = lambda *a, **k: (python or self.PY_OK)
        out, err = io.StringIO(), io.StringIO()
        try:
            with redirect_stdout(out), redirect_stderr(err):
                rc = cws.main(argv=list(argv), runner=runner, repo=repo)
        finally:
            cws.resolve_cargo, cws.resolve_python = saved
        return rc, out.getvalue(), err.getvalue()

    def test_both_bodies_green_exits_zero(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo = _fixture_repo(tmp)
            runner = FakeRunner()
            rc, out, err = self.run_main(repo, runner)
        self.assertEqual(rc, cws.RC_OK, f"out={out}\nerr={err}")
        self.assertEqual(
            len(runner.cargo_calls), 1, "the cargo body must actually be run"
        )
        self.assertEqual(
            len(runner.python_calls), 2,
            f"both discovered suites must be run: {runner.python_calls!r}",
        )

    def test_cargo_failure_prints_the_failing_name_and_its_target(self):
        """The entire reason this gate exists: a count told nobody anything."""
        with tempfile.TemporaryDirectory() as tmp:
            repo = _fixture_repo(tmp)
            runner = FakeRunner(cargo=_completed(rc=101, out=CARGO_ONE_RED))
            rc, out, err = self.run_main(repo, runner)
        both = out + err
        self.assertEqual(rc, cws.RC_CARGO_FAILURE, f"out={out}\nerr={err}")
        self.assertIn("detect::tests::bg2_x", both)
        self.assertIn("blastguard", both)
        self.assertIn("unittests src/lib.rs", both)

    def test_python_failure_prints_suite_and_failing_names(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo = _fixture_repo(tmp)
            runner = FakeRunner(
                suites={"scripts.test_beta": _completed(rc=1, out=UNITTEST_FAILED)}
            )
            rc, out, err = self.run_main(repo, runner)
        both = out + err
        self.assertEqual(rc, cws.RC_PYTHON_FAILURE, f"out={out}\nerr={err}")
        self.assertIn("scripts.test_beta", both)
        self.assertIn("test_gate_crate_absent", both)
        self.assertIn("test_absent_registry", both)

    def test_unresolvable_cargo_is_its_own_class(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo = _fixture_repo(tmp)
            runner = FakeRunner()
            rc, out, err = self.run_main(
                repo, runner,
                cargo=cws.Resolved(path=None, problem="no cargo on PATH and no "
                                                      "$HOME/.cargo/env"),
            )
        both = out + err
        self.assertEqual(rc, cws.RC_CARGO_UNDETERMINED, f"out={out}\nerr={err}")
        self.assertIn("cargo", both.lower())
        self.assertEqual(
            runner.cargo_calls, [],
            "an unresolvable cargo must not be launched anyway",
        )

    def test_unresolvable_python_is_its_own_class(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo = _fixture_repo(tmp)
            runner = FakeRunner()
            rc, out, err = self.run_main(
                repo, runner,
                python=cws.Resolved(path=None, problem="no python3 anywhere"),
            )
        self.assertEqual(rc, cws.RC_PYTHON_UNDETERMINED, f"out={out}\nerr={err}")
        self.assertIn("python", (out + err).lower())

    def test_suite_that_fails_to_import_is_python_undetermined(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo = _fixture_repo(tmp)
            runner = FakeRunner(
                suites={
                    "scripts.test_alpha": _completed(rc=1, out=UNITTEST_IMPORT_ERROR)
                }
            )
            rc, out, err = self.run_main(repo, runner)
        both = out + err
        self.assertEqual(rc, cws.RC_PYTHON_UNDETERMINED, f"out={out}\nerr={err}")
        self.assertIn("scripts.test_alpha", both)

    def test_discovery_finding_nothing_is_python_undetermined(self):
        """An empty scripts/ is not "nothing to run, therefore fine"."""
        with tempfile.TemporaryDirectory() as tmp:
            repo = _fixture_repo(tmp, suites=())
            runner = FakeRunner()
            rc, out, err = self.run_main(repo, runner)
        self.assertEqual(rc, cws.RC_PYTHON_UNDETERMINED, f"out={out}\nerr={err}")
        self.assertEqual(
            runner.python_calls, [], "there was nothing to run, so nothing ran"
        )

    def test_a_flag_is_refused_not_ignored(self):
        """`--skip` must not be silently dropped: a caller who believes a bypass
        exists and gets a clean exit has been told a lie."""
        for flag in ("--skip", "--warn-only"):
            with self.subTest(flag=flag), tempfile.TemporaryDirectory() as tmp:
                repo = _fixture_repo(tmp)
                runner = FakeRunner()
                rc, out, err = self.run_main(repo, runner, argv=[flag])
                both = out + err
                self.assertEqual(rc, cws.RC_ENVIRONMENT, f"out={out}\nerr={err}")
                self.assertIn(flag, both, "the refused argument must be echoed")
                self.assertEqual(
                    runner.calls, [],
                    "refused means nothing ran; running the bodies and then "
                    "exiting 5 would still have executed the run the flag "
                    "claimed to skip",
                )

    def test_repo_without_crates_is_an_environment_failure(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo = _fixture_repo(tmp, crates=False)
            runner = FakeRunner()
            rc, out, err = self.run_main(repo, runner)
        self.assertEqual(rc, cws.RC_ENVIRONMENT, f"out={out}\nerr={err}")

    def test_repo_without_scripts_is_an_environment_failure(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo = _fixture_repo(tmp, scripts=False)
            runner = FakeRunner()
            rc, out, err = self.run_main(repo, runner)
        self.assertEqual(rc, cws.RC_ENVIRONMENT, f"out={out}\nerr={err}")

    def test_timed_out_child_is_the_deadline_class(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo = _fixture_repo(tmp)
            runner = FakeRunner(
                cargo=_completed(rc=-9, out=CARGO_MIXED, timed_out=True)
            )
            rc, out, err = self.run_main(repo, runner)
        both = out + err
        self.assertEqual(rc, cws.RC_DEADLINE, f"out={out}\nerr={err}")
        self.assertIn("1800", both, "the deadline must be named")

    def test_ranking_puts_cargo_undetermined_over_python_failure(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo = _fixture_repo(tmp)
            runner = FakeRunner(
                cargo=_completed(rc=-9, out=CARGO_GREEN),
                suites={"scripts.test_beta": _completed(rc=1, out=UNITTEST_FAILED)},
            )
            rc, out, err = self.run_main(repo, runner)
        both = out + err
        self.assertEqual(
            rc, cws.RC_CARGO_UNDETERMINED,
            f"3 outranks 2 in 5 > 6 > 3 > 4 > 1 > 2.\nout={out}\nerr={err}",
        )
        # The ranking picks the exit CODE; it must not suppress the reporting.
        self.assertIn("scripts.test_beta", both,
                      "the python failure must still be printed in full")
        self.assertIn("test_gate_crate_absent", both)
        self.assertIn(
            "cargo", both.lower(),
            "the cargo class that WON the ranking must be printed too",
        )

    def test_python_undetermined_outranks_a_cargo_failure(self):
        """The ranking clause that `test_ranking_puts_cargo_undetermined_over_
        python_failure` above does NOT reach, and the most load-bearing one in
        the gate: 4 must beat 1.

        Added after a measured mutation survived the suite (2026-09-10). Moving
        `if cargo_report.failures: return RC_CARGO_FAILURE` above the two
        undetermined returns in `main()` left all 79 tests green, because the
        only ranking case here paired an undetermined CARGO body with a failing
        PYTHON body — and in that state `cargo_report.failures` is empty, so
        hoisting its return changes nothing. Nothing constructed a run where a
        named red and an undetermined body coexist, which is precisely the
        state the clause exists for.

        That state is not hypothetical: `cargo test --workspace` reds while one
        scripts/ suite fails to import is an ordinary Tuesday. Reporting it as
        "1 — cargo tests failed" sends the reader to the named red and says
        nothing about the suite that never executed, which is the lie in the
        module docstring's own words: "3 tests failed" printed over a body that
        never ran.
        """
        with tempfile.TemporaryDirectory() as tmp:
            repo = _fixture_repo(tmp)
            runner = FakeRunner(
                cargo=_completed(rc=101, out=CARGO_ONE_RED),
                suites={
                    "scripts.test_alpha": _completed(rc=1, out=UNITTEST_IMPORT_ERROR)
                },
            )
            rc, out, err = self.run_main(repo, runner)
        both = out + err
        self.assertEqual(
            rc, cws.RC_PYTHON_UNDETERMINED,
            "undetermined outranks failure: a suite that never imported must "
            "not be ranked below a red that at least ran.\n"
            f"out={out}\nerr={err}",
        )
        # ...and the ranking must not have suppressed either class.
        self.assertIn(
            "detect::tests::bg2_x", both, "the cargo red must still be named"
        )
        self.assertIn(
            "scripts.test_alpha", both, "the unimported suite must still be named"
        )

    def test_two_failures_with_no_undetermined_rank_cargo_first(self):
        """Control arm for the case above (anti-vacuity).

        Without it, a `main()` that returned RC_PYTHON_UNDETERMINED whenever
        anything at all was wrong would satisfy the assertion above while
        destroying the rest of the ranking. Here BOTH bodies produce ordinary
        named failures and no undetermined, so 1 must win over 2 — the same
        expected value on the pristine implementation and on the mutant, which
        is what makes it a control rather than a second copy of the test.
        """
        with tempfile.TemporaryDirectory() as tmp:
            repo = _fixture_repo(tmp)
            runner = FakeRunner(
                cargo=_completed(rc=101, out=CARGO_ONE_RED),
                suites={"scripts.test_alpha": _completed(rc=1, out=UNITTEST_FAILED)},
            )
            rc, out, err = self.run_main(repo, runner)
        both = out + err
        self.assertEqual(rc, cws.RC_CARGO_FAILURE, f"out={out}\nerr={err}")
        self.assertIn("detect::tests::bg2_x", both)
        self.assertIn("test_gate_crate_absent", both)

    def test_python_body_runs_even_when_cargo_is_undetermined(self):
        """Bailing out on the first bad class would hide everything below it,
        which is the other half of "every class that fired is printed"."""
        with tempfile.TemporaryDirectory() as tmp:
            repo = _fixture_repo(tmp)
            runner = FakeRunner(cargo=_completed(rc=-9, out=CARGO_GREEN))
            rc, out, err = self.run_main(repo, runner)
            self.assertEqual(
                len(runner.python_calls), 2,
                f"out={out}\nerr={err}\ncalls={runner.calls!r}",
            )
        self.assertEqual(rc, cws.RC_CARGO_UNDETERMINED)

    def test_suites_are_run_as_dash_m_unittest_not_as_a_script_path(self):
        """13 of the 23 real suites have no `unittest.main()` block, so
        `python3 scripts/test_x.py` runs ZERO tests and exits 0 — a fail-open
        hiding in the invocation style."""
        with tempfile.TemporaryDirectory() as tmp:
            repo = _fixture_repo(tmp)
            runner = FakeRunner()
            rc, out, err = self.run_main(repo, runner)
        self.assertEqual(rc, cws.RC_OK, f"out={out}\nerr={err}")
        for argv, _cwd, _deadline in runner.python_calls:
            self.assertIn("-m", argv, f"not a -m invocation: {argv!r}")
            self.assertIn("unittest", argv, f"not a -m unittest invocation: {argv!r}")
            self.assertTrue(
                any(a.startswith("scripts.test_") for a in argv),
                f"the suite must be named as a dotted module: {argv!r}",
            )
            self.assertFalse(
                any(a.endswith(".py") and "test_" in a for a in argv),
                f"a suite passed as a file path runs zero tests: {argv!r}",
            )

    def test_cargo_is_invoked_with_no_fail_fast(self):
        """`--no-fail-fast` is what makes the reported set the WHOLE set; without
        it the first red truncates the run and the remaining crates go dark."""
        with tempfile.TemporaryDirectory() as tmp:
            repo = _fixture_repo(tmp)
            runner = FakeRunner()
            rc, out, err = self.run_main(repo, runner)
        self.assertEqual(rc, cws.RC_OK, f"out={out}\nerr={err}")
        self.assertEqual(len(runner.cargo_calls), 1)
        argv = runner.cargo_calls[0][0]
        for token in cws.CARGO_ARGV:
            self.assertIn(token, argv, f"{token!r} missing from {argv!r}")

    def test_every_child_gets_a_deadline(self):
        """A child launched with deadline=None cannot expire, and the whole
        DEADLINE class becomes unreachable."""
        with tempfile.TemporaryDirectory() as tmp:
            repo = _fixture_repo(tmp)
            runner = FakeRunner()
            rc, out, err = self.run_main(repo, runner)
        self.assertEqual(rc, cws.RC_OK, f"out={out}\nerr={err}")
        self.assertTrue(runner.calls)
        for argv, _cwd, deadline in runner.calls:
            self.assertIsNotNone(deadline, f"no deadline for {argv!r}")
            self.assertGreater(deadline, 0, f"non-positive deadline for {argv!r}")

    def test_exit_codes_are_all_distinct(self):
        """CONSTANTS test, not a behavioural one: it reads the module's RC_*
        values, which already exist in the skeleton, so it was GREEN from the
        start and was never observed RED. It is here as a standing guard — two
        classes sharing a code makes the "distinct per failure CLASS" contract
        unenforceable, and the plugin-rollout script has already been bitten
        once by a traceback exiting 1 into RC_ROLLOUT's remediation."""
        codes = [
            cws.RC_OK, cws.RC_CARGO_FAILURE, cws.RC_PYTHON_FAILURE,
            cws.RC_CARGO_UNDETERMINED, cws.RC_PYTHON_UNDETERMINED,
            cws.RC_ENVIRONMENT, cws.RC_DEADLINE,
        ]
        self.assertEqual(len(set(codes)), len(codes), codes)


# ==========================================================================
# run_process — the three properties its docstring calls "none of them
# optional". These are the only cases below that launch a real child, because
# none of the three is observable any other way and leaving the highest-risk
# function at zero coverage would be the bigger sin. The child is always a
# throwaway `sys.executable -c` one-liner; cargo is never invoked. Budget for
# the whole class is ~2.5s, dominated by the process-group case, which has to
# outlive the grandchild it is proving dead.
# ==========================================================================

class RunProcess(unittest.TestCase):
    def test_child_stdin_is_devnull_even_when_the_parent_has_a_live_pipe(self):
        """The measured hang, reproduced in miniature.

        The parent's fd 0 is a pipe whose write end is held OPEN, so a child
        that INHERITS it blocks in read() until the deadline kills it — exactly
        what `(sleep 25) | cargo test ...` measured at 24.81s, and exactly what a
        pre-push hook's ref list on stdin would do. A child given /dev/null reads
        EOF at once.
        """
        code = "import sys; sys.stdout.write('STDIN=' + repr(sys.stdin.read()))"
        r_fd, w_fd = os.pipe()
        saved = os.dup(0)
        try:
            os.dup2(r_fd, 0)
            c = cws.run_process(
                [sys.executable, "-c", code], cwd=os.getcwd(), deadline=5.0
            )
        finally:
            os.dup2(saved, 0)
            for fd in (saved, r_fd, w_fd):
                try:
                    os.close(fd)
                except OSError:
                    pass
        self.assertFalse(
            c.timed_out,
            "the child blocked on the inherited pipe until the deadline — it "
            "did not get /dev/null",
        )
        self.assertEqual(c.rc, 0, c.out)
        self.assertIn("STDIN=''", c.out)

    def test_stderr_is_merged_into_stdout(self):
        """cargo prints `Running <target>` on stderr and libtest prints
        `test <name> ... FAILED` on stdout; their interleaving is the ONLY thing
        that ties a failing name to a target, so a split capture would make the
        attribution the parser does meaningless."""
        code = (
            "import sys; sys.stderr.write('ON-STDERR\\n'); sys.stderr.flush(); "
            "sys.stdout.write('ON-STDOUT\\n')"
        )
        c = cws.run_process(
            [sys.executable, "-c", code], cwd=os.getcwd(), deadline=30.0
        )
        self.assertEqual(c.rc, 0, c.out)
        self.assertIn("ON-STDERR", c.out)
        self.assertIn("ON-STDOUT", c.out)

    def test_deadline_expiry_is_flagged_and_bounded(self):
        c = cws.run_process(
            [sys.executable, "-c", "import time; time.sleep(30)"],
            cwd=os.getcwd(),
            deadline=0.4,
        )
        self.assertTrue(c.timed_out, "expiry must be flagged, not inferred")
        self.assertNotEqual(c.rc, 0, "a killed child did not succeed")
        self.assertEqual(c.deadline, 0.4, "the Completed must carry its limit")

    def test_deadline_kills_the_whole_process_group(self):
        """Killing only the parent leaves the test binaries running — one such
        orphan had to be killed by hand after 15 hours. The grandchild here
        stands in for a libtest binary: if only its parent is killed it survives
        and writes the marker."""
        with tempfile.TemporaryDirectory() as tmp:
            marker = Path(tmp) / "orphan-survived"
            grandchild = (
                f"import time; time.sleep(1.2); "
                f"open({str(marker)!r}, 'w').write('alive')"
            )
            code = (
                "import subprocess, sys, time; "
                f"subprocess.Popen([sys.executable, '-c', {grandchild!r}]); "
                "time.sleep(30)"
            )
            c = cws.run_process(
                [sys.executable, "-c", code], cwd=tmp, deadline=0.35
            )
            self.assertTrue(c.timed_out)
            time.sleep(1.6)
            self.assertFalse(
                marker.exists(),
                "the grandchild outlived the kill: only the direct child was "
                "signalled, so the process GROUP was not killed",
            )

    def test_unlaunchable_child_is_reported_not_raised(self):
        """"Never raises for a child that fails" — the caller resolves this to
        UNDETERMINED, which it cannot do if the exception escapes."""
        missing = str(Path(tempfile.gettempdir()) / "no-such-binary-2f4a9c")
        c = cws.run_process([missing, "--version"], cwd=os.getcwd(), deadline=5.0)
        self.assertIsNotNone(
            c.launch_error, "a child that could not start must say so"
        )
        self.assertFalse(c.timed_out)


# ==========================================================================
# Source-level property tests
# ==========================================================================

class NoBypass(unittest.TestCase):
    """SOURCE-LEVEL property tests, not behavioural ones.

    These read scripts/check-workspace-tests.py as text/AST and assert about
    what is written there. They can therefore pass on the un-implemented
    skeleton — a file with no code cannot contain a bypass flag — so they were
    NOT observed RED and prove nothing about behaviour. Their job is the
    opposite: to stay green forever, and to fail the moment someone adds the
    escape hatch the module docstring promises does not exist ("no bypass flag
    and no environment escape hatch ... inventing a second one here is the thing
    CLAUDE.md 4 forbids").
    """

    SRC = _SRC_PATH.read_text(encoding="utf-8")
    TREE = ast.parse(SRC)

    def _code_without_module_docstring(self):
        """The module docstring is prose ABOUT the absence of a bypass, so it is
        exempt; every other line is code or a comment on code."""
        doc = ast.get_docstring(self.TREE, clean=False)
        src = self.SRC
        if doc:
            src = src.replace(doc, "", 1)
        return src

    def test_no_bypass_flag_appears_in_the_code(self):
        body = self._code_without_module_docstring()
        offenders = re.findall(
            r"--(?:skip|force|warn[-_]only|no[-_]verify)\b", body
        )
        self.assertEqual(
            offenders, [],
            f"a bypass-shaped option appears in the source: {offenders}",
        )

    def test_no_argument_parser_that_could_accept_one(self):
        """`import argparse` is not itself a bypass, but this script takes NO
        arguments — an argument parser here has nothing legitimate to parse and
        is how a flag arrives later."""
        for node in ast.walk(self.TREE):
            if isinstance(node, ast.Import):
                for alias in node.names:
                    self.assertNotEqual(alias.name, "argparse")
            if isinstance(node, ast.ImportFrom):
                self.assertNotEqual(node.module, "argparse")

    def _func(self, name):
        for node in self.TREE.body:
            if isinstance(node, ast.FunctionDef) and node.name == name:
                return node
        self.fail(f"no top-level function `{name}` in {_SRC_PATH}")

    def _env_reads(self, node):
        hits = []
        for sub in ast.walk(node):
            if isinstance(sub, ast.Attribute) and sub.attr in ("environ", "getenv"):
                hits.append(ast.dump(sub))
            if isinstance(sub, ast.Name) and sub.id in ("environ", "getenv"):
                hits.append(sub.id)
        return hits

    def test_main_never_consults_an_environment_variable(self):
        """An env var that changes the verdict is the escape hatch by another
        name. `resolve_cargo` legitimately takes an injected `environ` (for
        $HOME); the verdict path must not read one at all."""
        hits = self._env_reads(self._func("main"))
        self.assertEqual(hits, [], f"main() reads the environment: {hits}")

    def test_the_verdict_functions_never_consult_the_environment(self):
        for name in ("cargo_verdict", "python_suite_verdict"):
            with self.subTest(function=name):
                hits = self._env_reads(self._func(name))
                self.assertEqual(hits, [], f"{name}() reads the environment: {hits}")

    def test_deadlines_are_constants_not_environment_reads(self):
        """"An env var that lengthens a deadline is an env var that disables
        this class" — the module says so; this pins it."""
        self.assertIsInstance(cws.CARGO_DEADLINE, float)
        self.assertIsInstance(cws.PYTHON_SUITE_DEADLINE, float)
        self.assertGreater(cws.CARGO_DEADLINE, 0)
        self.assertGreater(cws.PYTHON_SUITE_DEADLINE, 0)
        for node in self.TREE.body:
            if isinstance(node, ast.Assign):
                targets = [t.id for t in node.targets if isinstance(t, ast.Name)]
                if {"CARGO_DEADLINE", "PYTHON_SUITE_DEADLINE"} & set(targets):
                    self.assertIsInstance(
                        node.value, ast.Constant,
                        f"{targets} is not a literal constant",
                    )


if __name__ == "__main__":
    unittest.main()
