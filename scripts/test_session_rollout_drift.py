#!/usr/bin/env python3
"""Tests for scripts/session-rollout-drift.py (backlog b6a80342).

Stdlib-only (`unittest`), same shape as `scripts/test_gate_bypass.py`. Every
process launch goes through the injected `runner`, so nothing here runs the real
`scripts/check-plugin-rollout.py` and no test depends on this machine's actual
plugin cache.

    python3 scripts/test_session_rollout_drift.py

What these pin, in order of what would hurt most if it broke:

  1. rc=0 prints NOTHING. A notice on every session is wallpaper.
  2. Every non-zero class prints SOMETHING. The undetermined classes are the
     point: silence there would make "could not look" and "looked, it's clean"
     the same bytes.
  3. An UNKNOWN rc is undetermined, not clean — so a future exit class added to
     the checker cannot arrive here as silence.
"""

from __future__ import annotations

import importlib.util
import os
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
MODPATH = os.path.join(HERE, "session-rollout-drift.py")

_spec = importlib.util.spec_from_file_location("session_rollout_drift", MODPATH)
mod = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(mod)


class CleanIsSilent(unittest.TestCase):
    def test_rc_zero_says_nothing(self):
        self.assertIsNone(
            mod.classify(mod.RC_OK, "all 41 plugins rolled out"),
            "a clean check must print nothing; a notice every session is wallpaper",
        )

    def test_rc_zero_is_silent_even_with_noisy_output(self):
        # Guards against classifying on the OUTPUT instead of the exit code.
        self.assertIsNone(
            mod.classify(mod.RC_OK, "drift\nstale\nWARNING\nrollout"),
            "the exit code decides, not the prose",
        )


class DriftIsReported(unittest.TestCase):
    def test_rollout_drift_names_the_remedy(self):
        n = mod.classify(mod.RC_ROLLOUT, "plugin foo is at 0.1.1, source is 0.1.2")
        self.assertIsNotNone(n)
        self.assertIn("rollout-plugins.sh", n, "the notice must name the remedy")
        self.assertIn(
            "plugin foo is at 0.1.1",
            n,
            "the checker's own words must survive into the notice",
        )

    def test_enablement_and_retired_are_also_reported(self):
        for rc in (mod.RC_ENABLEMENT, mod.RC_RETIRED):
            with self.subTest(rc=rc):
                n = mod.classify(rc, "body")
                self.assertIsNotNone(n, f"rc={rc} must not be silent")
                self.assertIn("rollout-plugins.sh", n)


class UndeterminedIsNeverSilent(unittest.TestCase):
    """The load-bearing case. CLAUDE.md 3: cannot-determine is not clean."""

    def test_each_undetermined_class_is_announced(self):
        for rc in (mod.RC_UNVERIFIABLE, mod.RC_PARKED_CONFIG, mod.RC_RETIRED_CONFIG):
            with self.subTest(rc=rc):
                n = mod.classify(rc, "body")
                self.assertIsNotNone(n, f"rc={rc} must not be silent")
                self.assertIn(
                    "UNDETERMINED",
                    n,
                    "it must say it could not tell, not imply a verdict",
                )

    def test_launch_failure_is_undetermined_not_clean(self):
        n = mod.classify(None, "No such file or directory")
        self.assertIsNotNone(n, "a checker that never ran is not a clean checker")
        self.assertIn("UNDETERMINED", n)
        self.assertIn(
            "not the same as 'no drift'",
            n,
            "the notice must say what it does NOT mean",
        )

    def test_unknown_exit_code_is_undetermined_not_clean(self):
        n = mod.classify(99, "something new")
        self.assertIsNotNone(
            n, "an unrecognised rc must not resolve to the silent path"
        )
        self.assertIn("UNDETERMINED", n)
        self.assertIn("99", n, "the notice must name the code it did not recognise")


class BodyIsTruncatedNotSummarised(unittest.TestCase):
    def test_long_output_is_truncated_with_a_pointer(self):
        body = "\n".join(f"line{i}" for i in range(mod.MAX_BODY_LINES + 10))
        n = mod.classify(mod.RC_ROLLOUT, body)
        self.assertIn("line0", n)
        self.assertIn("more line(s)", n, "truncation must announce itself")
        self.assertNotIn(
            f"line{mod.MAX_BODY_LINES + 5}", n, "beyond the cap must be cut"
        )

    def test_empty_output_is_stated(self):
        n = mod.classify(mod.RC_ROLLOUT, "")
        self.assertIn("no output", n, "an empty body must be said, not shown as blank")


class RepoWithoutTheCheckerIsSilent(unittest.TestCase):
    def test_absent_checker_is_a_real_absence(self):
        def runner(argv, cwd, timeout):  # pragma: no cover - must not be called
            raise AssertionError("runner must not be invoked when checker is absent")

        with self.subTest("most repos are not this one"):
            self.assertIsNone(mod.notice_for_repo("/nonexistent-repo", runner=runner))

    def test_present_checker_is_actually_run(self):
        """Anti-vacuity control for the test above: when the checker DOES exist,
        the runner must be called — otherwise `notice_for_repo` returning None
        everywhere would satisfy the absent-case test."""
        calls = []

        def runner(argv, cwd, timeout):
            calls.append((argv, cwd))
            return mod.RC_ROLLOUT, "drifted"

        root = os.path.dirname(HERE)  # the repo root, which does ship the checker
        if not os.path.isfile(os.path.join(root, mod.CHECKER)):
            self.skipTest(f"{mod.CHECKER} not present at {root}")
        n = mod.notice_for_repo(root, runner=runner)
        self.assertEqual(len(calls), 1, "the checker must actually be invoked")
        self.assertIsNotNone(n)
        self.assertIn("drifted", n)


if __name__ == "__main__":
    unittest.main(verbosity=2)
