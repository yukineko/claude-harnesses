#!/usr/bin/env python3
"""Unit tests for scripts/check-fail-open-mutation.py.

Stdlib-only (`unittest`), no network, no real `cargo test` invocation (that
would take minutes per crate and depend on the ambient toolchain — instead
`run_crate_tests` is monkeypatched to return a controlled (exit_code, tail)
pair, so these tests exercise the harness's OWN logic: mutation apply/revert,
ambiguous-anchor refusal, dirty-file refusal, compile-failure classification,
and the caught/not-caught/inconclusive/error bookkeeping in `evaluate_scenario`
and the summary/exit-code functions `main()` is built from).

The real per-crate SCENARIOS (the actual mutations against real crate source)
are exercised against the real repo by `python3 scripts/check-fail-open-mutation.py`
itself (slow — full `cargo test -p <crate>` per scenario); a lightweight
`RealScenarioAnchors` check here instead pins the cheap, fast invariant that
matters between full runs: each scenario's `old` anchor still occurs EXACTLY
ONCE in the real target file, so a future refactor of that file doesn't
silently turn a mutation into a no-op (0 occurrences) or an ambiguous one (2+)
without anyone noticing until the next full run.
"""
import importlib.util
import subprocess
import tempfile
import unittest
from pathlib import Path

_HERE = Path(__file__).resolve().parent
_SPEC = importlib.util.spec_from_file_location(
    "check_fail_open_mutation", _HERE / "check-fail-open-mutation.py"
)
cfom = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(cfom)

REPO_ROOT = _HERE.parent


def _run(cmd, cwd):
    return subprocess.run(
        cmd, cwd=str(cwd), stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, check=True
    )


def _make_scratch_git_repo(tmp, filename="lib.rs", content="fn decide() -> bool {\n    true\n}\n"):
    """A real (but tiny, throwaway) git repo with one committed file, so
    `git status --porcelain` / `git checkout --` behave exactly as they would
    against the real repo, without touching any real crate source."""
    repo = Path(tmp)
    _run(["git", "init", "-q"], cwd=repo)
    _run(["git", "config", "user.email", "test@example.com"], cwd=repo)
    _run(["git", "config", "user.name", "test"], cwd=repo)
    path = repo / filename
    path.write_text(content, encoding="utf-8")
    _run(["git", "add", filename], cwd=repo)
    _run(["git", "commit", "-q", "-m", "init"], cwd=repo)
    return repo, path


class ApplyMutation(unittest.TestCase):
    def test_unique_anchor_mutates_the_file(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo, path = _make_scratch_git_repo(tmp)
            scenario = cfom.Scenario(
                crate="fake", file_rel="lib.rs", description="d",
                old="    true\n", new="    false\n",
            )
            old_repo = cfom.REPO
            cfom.REPO = repo
            try:
                err = cfom.apply_mutation(scenario)
            finally:
                cfom.REPO = old_repo
            self.assertIsNone(err)
            self.assertEqual(path.read_text(encoding="utf-8"), "fn decide() -> bool {\n    false\n}\n")

    def test_zero_occurrences_is_refused_not_guessed(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo, path = _make_scratch_git_repo(tmp)
            scenario = cfom.Scenario(
                crate="fake", file_rel="lib.rs", description="d",
                old="this text is not in the file\n", new="x\n",
            )
            old_repo = cfom.REPO
            cfom.REPO = repo
            try:
                err = cfom.apply_mutation(scenario)
            finally:
                cfom.REPO = old_repo
            self.assertIsNotNone(err)
            self.assertIn("found 0", err)
            # File must be untouched.
            self.assertEqual(path.read_text(encoding="utf-8"), "fn decide() -> bool {\n    true\n}\n")

    def test_ambiguous_anchor_is_refused_not_guessed(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo, path = _make_scratch_git_repo(
                tmp, content="fn a() -> bool {\n    true\n}\nfn b() -> bool {\n    true\n}\n"
            )
            scenario = cfom.Scenario(
                crate="fake", file_rel="lib.rs", description="d",
                old="    true\n", new="    false\n",
            )
            old_repo = cfom.REPO
            cfom.REPO = repo
            try:
                err = cfom.apply_mutation(scenario)
            finally:
                cfom.REPO = old_repo
            self.assertIsNotNone(err)
            self.assertIn("found 2", err)
            # File must be untouched -- an ambiguous anchor must never guess.
            self.assertEqual(
                path.read_text(encoding="utf-8"),
                "fn a() -> bool {\n    true\n}\nfn b() -> bool {\n    true\n}\n",
            )


class RevertMutation(unittest.TestCase):
    def test_revert_restores_byte_identical_content(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo, path = _make_scratch_git_repo(tmp)
            original = path.read_bytes()
            scenario = cfom.Scenario(
                crate="fake", file_rel="lib.rs", description="d",
                old="    true\n", new="    false\n",
            )
            old_repo = cfom.REPO
            cfom.REPO = repo
            try:
                self.assertIsNone(cfom.apply_mutation(scenario))
                self.assertNotEqual(path.read_bytes(), original)
                reverted = cfom.revert_mutation(scenario, original)
            finally:
                cfom.REPO = old_repo
            self.assertTrue(reverted)
            self.assertEqual(path.read_bytes(), original)

    def test_revert_runs_even_when_test_run_raises(self):
        """The finally-block contract in evaluate_scenario: an exception
        between apply and revert must not leave the mutation in place."""
        with tempfile.TemporaryDirectory() as tmp:
            repo, path = _make_scratch_git_repo(tmp)
            original = path.read_bytes()
            scenario = cfom.Scenario(
                crate="fake", file_rel="lib.rs", description="d",
                old="    true\n", new="    false\n",
            )
            old_repo, old_run_tests = cfom.REPO, cfom.run_crate_tests
            cfom.REPO = repo

            def boom(crate, timeout=600):
                raise RuntimeError("simulated failure between apply and revert")

            cfom.run_crate_tests = boom
            try:
                result = cfom.evaluate_scenario(scenario, timeout=5)
            finally:
                cfom.REPO = old_repo
                cfom.run_crate_tests = old_run_tests
            self.assertEqual(result.status, "error")
            self.assertTrue(result.revert_confirmed)
            self.assertEqual(path.read_bytes(), original)


class GitIsClean(unittest.TestCase):
    def test_clean_file_reads_as_clean(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo, _path = _make_scratch_git_repo(tmp)
            old_repo = cfom.REPO
            cfom.REPO = repo
            try:
                self.assertTrue(cfom.git_is_clean("lib.rs"))
            finally:
                cfom.REPO = old_repo

    def test_preexisting_dirty_file_refuses_to_mutate(self):
        """evaluate_scenario must refuse (status=error) rather than mutate a
        file that already has unrelated uncommitted changes -- clobbering
        someone's in-progress edit would be worse than skipping a scenario."""
        with tempfile.TemporaryDirectory() as tmp:
            repo, path = _make_scratch_git_repo(tmp)
            path.write_text("fn decide() -> bool {\n    true\n}\n// unrelated edit\n", encoding="utf-8")
            scenario = cfom.Scenario(
                crate="fake", file_rel="lib.rs", description="d",
                old="    true\n", new="    false\n",
            )
            old_repo = cfom.REPO
            cfom.REPO = repo
            try:
                self.assertFalse(cfom.git_is_clean("lib.rs"))
                result = cfom.evaluate_scenario(scenario, timeout=5)
            finally:
                cfom.REPO = old_repo
            self.assertEqual(result.status, "error")
            self.assertIn("uncommitted", result.detail)
            # revert_confirmed defaults True: nothing was ever mutated, so
            # there is nothing to revert -- the pre-existing dirty content
            # must be left exactly as it was, not touched by this script.
            self.assertTrue(result.revert_confirmed)
            self.assertEqual(
                path.read_text(encoding="utf-8"),
                "fn decide() -> bool {\n    true\n}\n// unrelated edit\n",
            )


class LooksLikeCompileFailure(unittest.TestCase):
    def test_rustc_error_code_is_a_compile_failure(self):
        self.assertTrue(cfom.looks_like_compile_failure("error[E0308]: mismatched types\n"))

    def test_could_not_compile_message_is_a_compile_failure(self):
        self.assertTrue(cfom.looks_like_compile_failure("error: could not compile `mutategate`\n"))

    def test_real_test_result_line_is_not_a_compile_failure(self):
        output = "running 3 tests\ntest foo ... FAILED\n\ntest result: FAILED. 2 passed; 1 failed;\n"
        self.assertFalse(cfom.looks_like_compile_failure(output))


class EvaluateScenarioOutcomes(unittest.TestCase):
    """Drives evaluate_scenario end-to-end against a scratch repo, with
    run_crate_tests monkeypatched to simulate cargo's outcome deterministically
    (no real cargo/toolchain dependency in this test)."""

    def _run_with_stubbed_tests(self, rc, tail):
        with tempfile.TemporaryDirectory() as tmp:
            repo, path = _make_scratch_git_repo(tmp)
            original = path.read_bytes()
            scenario = cfom.Scenario(
                crate="fake", file_rel="lib.rs", description="d",
                old="    true\n", new="    false\n",
            )
            old_repo, old_run_tests = cfom.REPO, cfom.run_crate_tests
            cfom.REPO = repo
            cfom.run_crate_tests = lambda crate, timeout=600: (rc, tail)
            try:
                result = cfom.evaluate_scenario(scenario, timeout=5)
            finally:
                cfom.REPO = old_repo
                cfom.run_crate_tests = old_run_tests
            self.assertEqual(path.read_bytes(), original, "must always revert")
            return result

    def test_nonzero_exit_with_test_result_line_is_caught(self):
        result = self._run_with_stubbed_tests(
            101, "running 1 test\ntest foo ... FAILED\n\ntest result: FAILED. 0 passed; 1 failed;\n"
        )
        self.assertEqual(result.status, "caught")
        self.assertTrue(result.red_confirmed)

    def test_zero_exit_is_not_caught(self):
        result = self._run_with_stubbed_tests(0, "test result: ok. 1 passed; 0 failed;\n")
        self.assertEqual(result.status, "not-caught")
        self.assertFalse(result.red_confirmed)

    def test_nonzero_exit_that_looks_like_a_compile_failure_is_inconclusive(self):
        result = self._run_with_stubbed_tests(101, "error[E0308]: mismatched types\n")
        self.assertEqual(result.status, "inconclusive")

    def test_timeout_is_inconclusive(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo, path = _make_scratch_git_repo(tmp)
            original = path.read_bytes()
            scenario = cfom.Scenario(
                crate="fake", file_rel="lib.rs", description="d",
                old="    true\n", new="    false\n",
            )
            old_repo, old_run_tests = cfom.REPO, cfom.run_crate_tests
            cfom.REPO = repo
            cfom.run_crate_tests = lambda crate, timeout=600: (None, "timed out")
            try:
                result = cfom.evaluate_scenario(scenario, timeout=5)
            finally:
                cfom.REPO = old_repo
                cfom.run_crate_tests = old_run_tests
            self.assertEqual(result.status, "inconclusive")
            self.assertEqual(path.read_bytes(), original)


class SelectScenarios(unittest.TestCase):
    def test_no_filter_returns_all(self):
        self.assertEqual(cfom.select_scenarios(None), cfom.SCENARIOS)

    def test_filter_by_crate_returns_only_that_crates_scenarios(self):
        result = cfom.select_scenarios("mutategate")
        self.assertTrue(result)
        self.assertTrue(all(s.crate == "mutategate" for s in result))


class SummaryAndExitCode(unittest.TestCase):
    def _fake_result(self, status, revert_confirmed=True):
        scenario = cfom.Scenario(crate="c", file_rel="f", description="d", old="a", new="b")
        r = cfom.ScenarioResult(scenario)
        r.status = status
        r.revert_confirmed = revert_confirmed
        return r

    def test_all_caught_exits_zero(self):
        results = [self._fake_result("caught"), self._fake_result("caught")]
        summary = cfom.build_summary(results, total_scenarios=2)
        self.assertEqual(cfom.exit_code_for(summary), 0)

    def test_any_not_caught_exits_one(self):
        results = [self._fake_result("caught"), self._fake_result("not-caught")]
        summary = cfom.build_summary(results, total_scenarios=2)
        self.assertEqual(cfom.exit_code_for(summary), 1)
        self.assertEqual(len(summary["not_caught"]), 1)

    def test_any_inconclusive_exits_one(self):
        results = [self._fake_result("inconclusive")]
        summary = cfom.build_summary(results, total_scenarios=1)
        self.assertEqual(cfom.exit_code_for(summary), 1)

    def test_revert_failure_exits_one_even_if_caught(self):
        """A caught mutation whose revert failed must still fail the run --
        a successful test-catch does not excuse leaving the mutation live."""
        results = [self._fake_result("caught", revert_confirmed=False)]
        summary = cfom.build_summary(results, total_scenarios=1)
        self.assertEqual(cfom.exit_code_for(summary), 1)
        self.assertEqual(len(summary["revert_failed"]), 1)


class RealScenarioAnchors(unittest.TestCase):
    """Cheap (no cargo test) regression guard: each registered scenario's
    mutation anchor must still occur EXACTLY ONCE in the real target file, so
    a future refactor silently turning a mutation into a no-op or an
    ambiguous match is caught immediately, without waiting for someone to run
    the slow full harness."""

    def test_every_scenario_anchor_is_unique_in_the_real_file(self):
        for s in cfom.SCENARIOS:
            path = REPO_ROOT / s.file_rel
            text = path.read_text(encoding="utf-8")
            count = text.count(s.old)
            self.assertEqual(
                count, 1,
                f"{s.crate}: anchor for {s.file_rel!r} now occurs {count} time(s), "
                "expected exactly 1 (source has drifted -- update the scenario)",
            )

    def test_every_gate_crate_has_at_least_one_scenario(self):
        covered = {s.crate for s in cfom.SCENARIOS}
        self.assertEqual(covered, set(cfom.GATE_CRATES))


if __name__ == "__main__":
    unittest.main()
