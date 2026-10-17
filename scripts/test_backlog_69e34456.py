#!/usr/bin/env python3
"""Backlog 69e34456 — RED repro.

`check-workspace-tests.py --python-only` reports four suites as UNDETERMINED:

  test_backlog_0c277117 / test_backlog_b63cbdf0: `FAILED (unexpected
      successes=1)` — an @expectedFailure pin whose defect is now fixed. The
      summary says FAILED but there is no FAIL:/ERROR: line, so the gate cannot
      name the red.
  test_backlog_1116bee1 / test_backlog_f6c60f58: setUpClass skips unless
      RUN_MUTATION_REPROS=1, so the suite reports `Ran 0 tests`.

Each test below runs the suite exactly as the gate's python body does
(`python3 -m unittest scripts.<module>`, cwd=repo, stdin=/dev/null, via the
gate's own run_process) and folds it through the gate's own
parse_unittest_output + python_suite_verdict. The property asserted is the
one the item claims is violated: the suite yields a determined, clean verdict.

Run: python3 -m pytest --runxfail scripts/test_backlog_69e34456.py
"""

import importlib.util
import os
import sys
import unittest
from pathlib import Path

SRC = Path(__file__).resolve().parent.parent
GATE_PATH = SRC / "scripts" / "check-workspace-tests.py"


def _load_gate():
    spec = importlib.util.spec_from_file_location("check_workspace_tests", GATE_PATH)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


GATE = _load_gate()


class WorkspacePythonBodySuites(unittest.TestCase):
    def _verdict(self, module):
        env = dict(os.environ)
        env.pop("RUN_MUTATION_REPROS", None)
        argv = (sys.executable, "-m", "unittest", "scripts." + module)
        completed = GATE.run_process(argv, str(SRC), GATE.PYTHON_SUITE_DEADLINE, env)
        parse = GATE.parse_unittest_output(completed.out)
        return GATE.python_suite_verdict("scripts." + module, completed, parse), completed

    def _assert_clean(self, module):
        report, completed = self._verdict(module)
        self.assertEqual(
            (report.failures, report.undetermined, report.deadline),
            ((), (), ()),
            "check-workspace-tests verdict for %s is not clean:\n%r\n--- output ---\n%s"
            % (module, report, completed.out),
        )

    def test_0c277117_suite_verdict_is_clean(self):
        self._assert_clean("test_backlog_0c277117")

    def test_b63cbdf0_suite_verdict_is_clean(self):
        self._assert_clean("test_backlog_b63cbdf0")

    def test_1116bee1_suite_verdict_is_clean(self):
        self._assert_clean("test_backlog_1116bee1")

    @unittest.expectedFailure
    def test_f6c60f58_suite_verdict_is_clean(self):
        self._assert_clean("test_backlog_f6c60f58")


if __name__ == "__main__":
    unittest.main()
