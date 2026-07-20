#!/usr/bin/env python3
"""Unit tests for check-ci-red.py.

Hermetic: every test builds its own run list. The `gh` boundary (`_gh_json`) is
the only impure part and is always stubbed.

The tests are organised around the two rules the checker must obey, because
those — not the formatting — are what regressions break:

  * A "could not tell" must never present as an all-clear. Asserting only on
    exit codes is not enough for this, because RC_OK and a fail-soft skip were
    once the same value; the tests therefore assert the exit code AND what the
    reader is actually shown.
  * Anything that might be a problem must be reported, including a streak whose
    length cannot be bounded.
"""

import contextlib
import importlib.util
import io
import json
import pathlib
import subprocess
import unittest
from datetime import datetime, timedelta, timezone

_p = pathlib.Path(__file__).with_name("check-ci-red.py")
_spec = importlib.util.spec_from_file_location("check_ci_red", _p)
m = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(m)


def run(conclusion, status="completed", created="2026-07-16T00:00:00Z", url="u"):
    return {"conclusion": conclusion, "status": status, "createdAt": created, "url": url}


@contextlib.contextmanager
def captured():
    out, err = io.StringIO(), io.StringIO()
    with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
        yield out, err


class RedStreak(unittest.TestCase):
    def test_no_runs(self):
        self.assertEqual(m.red_streak([]), (0, None, True))

    def test_all_green(self):
        streak, idx, exhausted = m.red_streak([run("success")] * 5)
        self.assertEqual((streak, idx, exhausted), (0, None, False))

    def test_leading_failures_bounded_by_a_green(self):
        self.assertEqual(m.red_streak([run("failure")] * 3 + [run("success")]), (3, 2, False))

    def test_all_failures_is_exhausted(self):
        streak, idx, exhausted = m.red_streak([run("failure")] * 4)
        self.assertEqual((streak, idx), (4, 3))
        self.assertTrue(exhausted)

    def test_green_breaks_the_streak(self):
        runs = [run("failure"), run("success"), run("failure"), run("failure")]
        self.assertEqual(m.red_streak(runs), (1, 0, False))

    def test_latest_green_means_not_red(self):
        self.assertEqual(m.red_streak([run("success")] + [run("failure")] * 9)[0], 0)

    def test_cancelled_is_skipped_not_treated_as_green(self):
        # Counting a cancelled run as a recovery would report a streak of 1 for
        # a workflow that has actually failed 3 times running.
        runs = [run("failure"), run("cancelled"), run("failure"), run("failure"), run("success")]
        self.assertEqual(m.red_streak(runs)[0], 3)

    def test_oldest_index_accounts_for_neutrals_inside_the_streak(self):
        # The off-by-one this return value exists to fix: the count skips
        # neutrals but the index must not, or the reported start date lands one
        # slot too recent and understates how long the workflow has been red.
        runs = [
            run("failure", created="2026-07-20T00:00:00Z"),
            run("cancelled", created="2026-07-19T00:00:00Z"),
            run("failure", created="2026-07-18T00:00:00Z"),
            run("failure", created="2026-07-17T00:00:00Z"),
            run("success", created="2026-07-16T00:00:00Z"),
        ]
        streak, idx, _ = m.red_streak(runs)
        self.assertEqual(streak, 3)
        self.assertEqual(runs[idx]["createdAt"][:10], "2026-07-17")

    def test_exhausted_accounts_for_neutrals(self):
        # The window is fully consumed, so the streak is a lower bound even
        # though `streak` (2) is less than len(runs) (3).
        self.assertTrue(m.red_streak([run("failure"), run("cancelled"), run("failure")])[2])

    def test_in_flight_run_is_skipped(self):
        runs = [run(None, status="in_progress"), run("failure"), run("failure")]
        self.assertEqual(m.red_streak(runs)[0], 2)

    def test_timed_out_and_startup_failure_count_as_red(self):
        self.assertEqual(m.red_streak([run("timed_out"), run("startup_failure")])[0], 2)


class Classify(unittest.TestCase):
    def test_green(self):
        self.assertEqual(m.classify("w", [run("success")] * 3, 3)[0], "green")

    def test_chronic_when_bounded_streak_meets_threshold(self):
        bucket, row = m.classify("w", [run("failure")] * 3 + [run("success")], 3)
        self.assertEqual(bucket, "chronic")
        self.assertFalse(row["truncated"])

    def test_chronic_when_lower_bound_already_meets_threshold(self):
        # Exhausted, but >= threshold already: the verdict is safe (it is
        # chronic, possibly worse), so this is NOT undetermined.
        bucket, row = m.classify("w", [run("failure")] * 5, 3)
        self.assertEqual(bucket, "chronic")
        self.assertTrue(row["truncated"])

    def test_fresh_only_when_the_streak_is_bounded(self):
        bucket, _ = m.classify("w", [run("failure"), run("success")], 3)
        self.assertEqual(bucket, "fresh")

    def test_short_unbounded_streak_is_undetermined_not_fresh(self):
        # The core F1 regression. A rarely-run workflow with 1 fetched run, red,
        # could have been failing for a year. Calling it "recently red" — the
        # benign class — is what let such a workflow rot unseen.
        bucket, row = m.classify("rare", [run("failure")], 3)
        self.assertEqual(bucket, "undetermined")
        self.assertIn("lower bound", row["why"])

    def test_workflow_with_no_runs_is_undetermined_not_green(self):
        bucket, row = m.classify("never-ran", [], 3)
        self.assertEqual(bucket, "undetermined")
        self.assertIn("never run", row["why"])

    def test_since_uses_the_neutral_corrected_index(self):
        runs = [
            run("failure", created="2026-07-20T00:00:00Z"),
            run("cancelled", created="2026-07-19T00:00:00Z"),
            run("failure", created="2026-07-18T00:00:00Z"),
            run("failure", created="2026-07-17T00:00:00Z"),
            run("success", created="2026-07-16T00:00:00Z"),
        ]
        _, row = m.classify("w", runs, 3)
        self.assertEqual(row["since"], "2026-07-17")

    def test_truncated_marker_shown_for_lower_bounds_only(self):
        _, bounded = m.classify("w", [run("failure")] * 3 + [run("success")], 3)
        _, unbounded = m.classify("w", [run("failure")] * 3, 3)
        self.assertNotIn(">=", m.describe(bounded))
        self.assertIn(">=3", m.describe(unbounded))


class DaysSince(unittest.TestCase):
    def test_parses_z_suffix(self):
        ts = (datetime.now(timezone.utc) - timedelta(days=3)).strftime("%Y-%m-%dT%H:%M:%SZ")
        self.assertEqual(m.days_since(ts), 3)

    def test_missing_or_malformed_is_none(self):
        for bad in ("", None, "not-a-date"):
            self.assertIsNone(m.days_since(bad))

    def test_future_timestamp_clamped_to_zero(self):
        ts = (datetime.now(timezone.utc) + timedelta(days=2)).strftime("%Y-%m-%dT%H:%M:%SZ")
        self.assertEqual(m.days_since(ts), 0)


class GhBoundaryIsFailClosed(unittest.TestCase):
    """Every unavailability must raise, never return an empty/green result."""

    def setUp(self):
        self._which, self._run = m.shutil.which, m.subprocess.run
        m.shutil.which = lambda _n: "/usr/bin/gh"

    def tearDown(self):
        m.shutil.which, m.subprocess.run = self._which, self._run

    def test_missing_gh(self):
        m.shutil.which = lambda _n: None
        with self.assertRaises(m.Unavailable):
            m._gh_json(["run", "list"])

    def test_nonzero_exit(self):
        m.subprocess.run = lambda *a, **k: subprocess.CompletedProcess(a, 1, "", "not authenticated")
        with self.assertRaisesRegex(m.Unavailable, "not authenticated"):
            m._gh_json(["run", "list"])

    def test_timeout(self):
        def boom(*a, **k):
            raise subprocess.TimeoutExpired("gh", 1)
        m.subprocess.run = boom
        with self.assertRaisesRegex(m.Unavailable, "timed out"):
            m._gh_json(["run", "list"])

    def test_oserror(self):
        def boom(*a, **k):
            raise OSError("permission denied")
        m.subprocess.run = boom
        with self.assertRaisesRegex(m.Unavailable, "could not run gh"):
            m._gh_json(["run", "list"])

    def test_malformed_json(self):
        m.subprocess.run = lambda *a, **k: subprocess.CompletedProcess(a, 0, "{not json", "")
        with self.assertRaisesRegex(m.Unavailable, "unparseable"):
            m._gh_json(["run", "list"])

    def test_non_list_payload(self):
        m.subprocess.run = lambda *a, **k: subprocess.CompletedProcess(a, 0, '{"a":1}', "")
        with self.assertRaisesRegex(m.Unavailable, "unexpected JSON shape"):
            m._gh_json(["run", "list"])

    def test_happy_path(self):
        m.subprocess.run = lambda *a, **k: subprocess.CompletedProcess(a, 0, json.dumps([{"x": 1}]), "")
        self.assertEqual(m._gh_json(["run", "list"]), [{"x": 1}])


class IsManualOnly(unittest.TestCase):
    """Recognising a known-by-design state, NOT suppressing an unknown one.

    Tri-state: True (dispatch-only), False (has a real trigger), None (could not
    tell). None must never be written as False — `assertFalse(None)` passes, so
    every negative case here asserts `is None` or `is False` explicitly rather
    than using assertTrue/assertFalse, which cannot tell the two apart. That
    laxity is what let the whole feature sit dead on CI.
    """

    def setUp(self):
        # These cases are ABOUT parsing YAML, so a missing PyYAML makes every one
        # of them vacuous (is_manual_only would answer None throughout and the
        # negative assertions would still pass). Skip loudly instead: CI installs
        # PyYAML, and a silent green here is what hid the defect before.
        try:
            import yaml  # noqa: F401
        except ImportError:  # pragma: no cover
            self.skipTest("PyYAML not installed; trigger-parsing cases cannot run")

    def _wf(self, body):
        import tempfile
        f = tempfile.NamedTemporaryFile("w", suffix=".yml", delete=False)
        f.write(body)
        f.close()
        return f.name

    def test_dispatch_only_mapping(self):
        self.assertIs(m.is_manual_only(self._wf("on:\n  workflow_dispatch:\njobs: {}\n")), True)

    def test_dispatch_only_scalar(self):
        self.assertIs(m.is_manual_only(self._wf("on: workflow_dispatch\njobs: {}\n")), True)

    def test_dispatch_only_list(self):
        self.assertIs(m.is_manual_only(self._wf("on: [workflow_dispatch]\njobs: {}\n")), True)

    def test_dispatch_plus_push_is_not_manual_only(self):
        body = "on:\n  workflow_dispatch:\n  push:\n    branches: [main]\njobs: {}\n"
        self.assertIs(m.is_manual_only(self._wf(body)), False)

    def test_schedule_only_is_not_manual_only(self):
        # A scheduled workflow IS expected to run; absence of runs is a problem.
        self.assertIs(
            m.is_manual_only(self._wf("on:\n  schedule:\n    - cron: '0 0 * * *'\n")), False
        )

    def test_quoted_on_key_still_recognised(self):
        # PyYAML resolves a bare `on` to boolean True; a quoted "on" stays a
        # string. Both must work.
        self.assertIs(m.is_manual_only(self._wf('"on":\n  workflow_dispatch:\njobs: {}\n')), True)

    # --- the cases that must be UNDETERMINED, not a verdict ---

    def test_unreadable_path_is_undetermined_not_a_verdict(self):
        self.assertIsNone(m.is_manual_only("/nonexistent/nope.yml"))

    def test_empty_path_is_undetermined_not_a_verdict(self):
        self.assertIsNone(m.is_manual_only(""))

    def test_malformed_yaml_is_undetermined_not_a_verdict(self):
        self.assertIsNone(m.is_manual_only(self._wf("on: [unclosed\n")))

    def test_missing_trigger_block_is_undetermined_not_a_verdict(self):
        self.assertIsNone(m.is_manual_only(self._wf("jobs: {}\n")))

    def test_explicit_null_trigger_is_undetermined_not_a_verdict(self):
        # `.get("on", .get(True))` returned None here and fell to the `False`
        # tail, reporting "has a real trigger" about a workflow with no readable
        # trigger at all.
        self.assertIsNone(m.is_manual_only(self._wf("on:\njobs: {}\n")))

    def test_real_repo_bench_regression_is_manual_only(self):
        p = pathlib.Path(__file__).resolve().parent.parent / ".github/workflows/bench-regression.yml"
        if p.exists():
            self.assertIs(m.is_manual_only(str(p)), True)

    def test_real_repo_gate_crates_sync_is_not_manual_only(self):
        p = pathlib.Path(__file__).resolve().parent.parent / ".github/workflows/gate-crates-sync.yml"
        if p.exists():
            self.assertIs(m.is_manual_only(str(p)), False)


class UnreadableTriggerIsUndetermined(unittest.TestCase):
    """A workflow whose trigger could not be read must not be silently judged.

    Both collapses are wrong, in opposite directions: treating it as manual-only
    hides a genuinely rotting workflow, and treating it as judgeable can invent a
    chronic-red finding about one that was never meant to run on this branch.
    """

    def setUp(self):
        self._lw, self._fr = m.list_workflows, m.fetch_runs
        m.fetch_runs = lambda *a, **k: []

    def tearDown(self):
        m.list_workflows, m.fetch_runs = self._lw, self._fr

    def test_unreadable_workflow_lands_in_undetermined_with_a_reason(self):
        m.list_workflows = lambda: ([], [], [{"id": 5, "name": "mystery", "path": "m.yml"}])
        buckets = m.collect("main", 25, 3, 45)
        names = [u["workflow"] for u in buckets["undetermined"]]
        self.assertIn("mystery", names)
        self.assertNotIn("mystery", buckets["manual_only"])
        why = next(u["why"] for u in buckets["undetermined"] if u["workflow"] == "mystery")
        self.assertTrue(why, "an undetermined workflow must carry a reason")

    def test_missing_pyyaml_is_named_as_the_missing_capability(self):
        # The failure mode that hid this: no PyYAML on the CI image made EVERY
        # workflow unreadable at once. The reader must be told that, not handed N
        # identical mystery lines.
        real = m.manual_only_unavailable_reason
        m.manual_only_unavailable_reason = lambda: "PyYAML is not installed, so ..."
        try:
            m.list_workflows = lambda: ([], [], [{"id": 5, "name": "w", "path": "w.yml"}])
            buckets = m.collect("main", 25, 3, 45)
            self.assertIn("PyYAML", buckets["undetermined"][0]["why"])
        finally:
            m.manual_only_unavailable_reason = real

    def test_an_unreadable_workflow_alone_is_not_a_clean_bill(self):
        # Exit code must not be RC_OK when the only thing we saw was unreadable.
        m.list_workflows = lambda: ([], [], [{"id": 5, "name": "w", "path": "w.yml"}])
        buckets = m.collect("main", 25, 3, 45)
        self.assertNotEqual(m.report(buckets, "main", 3), m.RC_OK)


class ListWorkflows(unittest.TestCase):
    def setUp(self):
        self._gh, self._mo = m._gh_json, m.is_manual_only
        m.is_manual_only = lambda _p: False

    def tearDown(self):
        m._gh_json, m.is_manual_only = self._gh, self._mo

    def test_disabled_workflows_excluded(self):
        m._gh_json = lambda *a, **k: [
            {"id": 1, "name": "a", "state": "active"},
            {"id": 2, "name": "b", "state": "disabled_manually"},
        ]
        judgeable, manual, unreadable = m.list_workflows()
        self.assertEqual([w["name"] for w in judgeable], ["a"])
        self.assertEqual(manual, [])

    def test_manual_only_workflows_are_separated_not_dropped(self):
        m._gh_json = lambda *a, **k: [
            {"id": 1, "name": "a", "path": "a.yml", "state": "active"},
            {"id": 2, "name": "manual", "path": "m.yml", "state": "active"},
        ]
        m.is_manual_only = lambda p: p == "m.yml"
        judgeable, manual, unreadable = m.list_workflows()
        self.assertEqual([w["name"] for w in judgeable], ["a"])
        self.assertEqual([w["name"] for w in manual], ["manual"])

    def test_non_object_entry_raises(self):
        m._gh_json = lambda *a, **k: ["oops"]
        with self.assertRaises(m.Unavailable):
            m.list_workflows()

    def test_entry_missing_id_raises(self):
        m._gh_json = lambda *a, **k: [{"name": "a", "state": "active"}]
        with self.assertRaises(m.Unavailable):
            m.list_workflows()


class Collect(unittest.TestCase):
    def setUp(self):
        self._lw, self._fr = m.list_workflows, m.fetch_runs

    def tearDown(self):
        m.list_workflows, m.fetch_runs = self._lw, self._fr

    def test_no_active_workflows_raises_rather_than_reporting_clean(self):
        m.list_workflows = lambda: ([], [], [])
        with self.assertRaises(m.Unavailable):
            m.collect("main", 25, 3)

    def test_manual_only_names_are_carried_through_for_reporting(self):
        m.list_workflows = lambda: ([], [{"id": 9, "name": "manual"}], [])
        b = m.collect("main", 25, 3)
        self.assertEqual(b["manual_only"], ["manual"])

    def test_per_workflow_failure_is_undetermined_not_dropped(self):
        m.list_workflows = lambda: ([{"id": 1, "name": "a"}, {"id": 2, "name": "b"}], [], [])

        def fetch(wid, branch, depth):
            if wid == 2:
                raise m.Unavailable("boom")
            return [run("success")]
        m.fetch_runs = fetch
        b = m.collect("main", 25, 3)
        self.assertEqual(b["green"], ["a"])
        self.assertEqual([r["workflow"] for r in b["undetermined"]], ["b"])

    def test_unexpected_exception_is_undetermined_not_a_crash(self):
        m.list_workflows = lambda: ([{"id": 1, "name": "a"}], [], [])

        def fetch(*a):
            raise TypeError("bad payload")
        m.fetch_runs = fetch
        b = m.collect("main", 25, 3)
        self.assertEqual(len(b["undetermined"]), 1)
        self.assertIn("TypeError", b["undetermined"][0]["why"])

    def test_exhausted_budget_marks_remaining_undetermined_not_skipped(self):
        m.list_workflows = lambda: ([{"id": i, "name": f"w{i}"} for i in range(3)], [], [])
        m.fetch_runs = lambda *a: [run("success")]
        # A clock already past the deadline on every call.
        b = m.collect("main", 25, 3, budget=0, now=lambda: 10.0)
        self.assertEqual(len(b["undetermined"]), 3)
        self.assertTrue(all("budget" in r["why"] for r in b["undetermined"]))


class ReportAndExitCodes(unittest.TestCase):
    """The distinction between 'fine' and 'could not tell' must reach the reader.

    Asserting exit codes alone is insufficient here: an earlier version returned
    0 for both, so tests that checked only the code would have passed with the
    central requirement fully inverted.
    """

    def _report(self, buckets):
        full = {"green": [], "chronic": [], "fresh": [], "undetermined": []}
        full.update(buckets)
        with captured() as (out, err):
            rc = m.report(full, "main", 3)
        return rc, out.getvalue()

    def test_all_green_is_rc_ok_and_says_so(self):
        rc, text = self._report({"green": ["a", "b"]})
        self.assertEqual(rc, m.RC_OK)
        self.assertIn("OK:", text)
        self.assertIn("2 judgeable workflow(s)", text)

    def test_chronic_is_rc_chronic(self):
        rc, text = self._report({"chronic": [{"workflow": "a", "streak": 5, "truncated": False,
                                              "since": "2026-07-10", "days": 5, "url": "u"}]})
        self.assertEqual(rc, m.RC_CHRONIC)
        self.assertIn("CHRONICALLY RED", text)

    def test_undetermined_alone_is_rc_undetermined_and_never_prints_ok(self):
        rc, text = self._report({"green": ["a"],
                                 "undetermined": [{"workflow": "b", "why": "never ran"}]})
        self.assertEqual(rc, m.RC_UNDETERMINED)
        self.assertIn("UNDETERMINED", text)
        self.assertIn("NOT an all-clear", text)
        self.assertNotIn("OK:", text)

    def test_fresh_alone_is_rc_ok(self):
        rc, text = self._report({"green": ["a"],
                                 "fresh": [{"workflow": "b", "streak": 1, "truncated": False,
                                            "since": "2026-07-20", "days": 0, "url": "u"}]})
        self.assertEqual(rc, m.RC_OK)
        self.assertIn("OK:", text)

    def test_chronic_wins_over_undetermined_but_both_are_shown(self):
        rc, text = self._report({
            "chronic": [{"workflow": "a", "streak": 9, "truncated": True,
                         "since": "2026-07-01", "days": 19, "url": "u"}],
            "undetermined": [{"workflow": "b", "why": "never ran"}],
        })
        self.assertEqual(rc, m.RC_CHRONIC)
        self.assertIn("CHRONICALLY RED", text)
        self.assertIn("UNDETERMINED", text)

    def test_exit_codes_are_three_distinct_values(self):
        self.assertEqual(len({m.RC_OK, m.RC_CHRONIC, m.RC_UNDETERMINED}), 3)


class Main(unittest.TestCase):
    def setUp(self):
        self._collect = m.collect

    def tearDown(self):
        m.collect = self._collect

    def _main(self, argv):
        import sys
        old = sys.argv
        sys.argv = ["check-ci-red.py"] + argv
        try:
            with captured() as (out, err):
                rc = m.main()
            return rc, out.getvalue(), err.getvalue()
        finally:
            sys.argv = old

    def test_unavailable_is_undetermined_not_ok(self):
        def boom(*a, **k):
            raise m.Unavailable("no gh")
        m.collect = boom
        rc, out, err = self._main([])
        self.assertEqual(rc, m.RC_UNDETERMINED)
        self.assertIn("NOT an all-clear", err)
        self.assertNotIn("OK:", out)

    def test_crash_is_undetermined_not_chronic(self):
        # The bug: an uncaught exception exited 1, the same code as
        # "chronically red", so the hook narrated a traceback as a CI verdict.
        def boom(*a, **k):
            raise RuntimeError("kaboom")
        m.collect = boom
        rc, out, err = self._main([])
        self.assertEqual(rc, m.RC_UNDETERMINED)
        self.assertNotEqual(rc, m.RC_CHRONIC)
        self.assertIn("NOT an all-clear", err)

    def test_nonsense_threshold_is_undetermined_not_silent_zero(self):
        m.collect = lambda *a, **k: {"green": [], "chronic": [], "fresh": [], "undetermined": []}
        rc, out, err = self._main(["--chronic", "0"])
        self.assertEqual(rc, m.RC_UNDETERMINED)
        self.assertIn("nothing was checked", err)

    def test_nonsense_depth_is_undetermined(self):
        rc, out, err = self._main(["--depth", "0"])
        self.assertEqual(rc, m.RC_UNDETERMINED)
        self.assertIn("nothing was checked", err)

    def test_clean_run_is_ok(self):
        m.collect = lambda *a, **k: {"green": ["a"], "chronic": [], "fresh": [], "undetermined": []}
        rc, out, _ = self._main([])
        self.assertEqual(rc, m.RC_OK)
        self.assertIn("OK:", out)


if __name__ == "__main__":
    unittest.main(verbosity=1)
