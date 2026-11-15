#!/usr/bin/env python3
"""RED: test_fail_open_mutation_leaves_no_artifacts.py passes vacuously.

Observed 2026-11-14 (main 670f82987, side finding of condukt run
run-20261114-163341-69351 task b793fddc): the stub in
MutationRunArtifacts._drive is defined as `stub(crate, timeout=600)`, but
check-fail-open-mutation.py calls
`run_crate_tests(scenario.crate, timeout=timeout, test_args=scenario.test_args())`.
The call raises TypeError, the scenario resolves to status `error`, and the stub
(the part that plays proptest and writes the untracked artifact) never runs. The
artifact test then sees no leaked file and passes without checking anything.

This pins that the driver really reaches the stub: the run output must not
report the stub's call-shape TypeError. Repro:
    python3 -m pytest --runxfail scripts/test_fail_open_mutation_leaves_no_artifacts_stub_invoked_20261114.py
"""
import importlib.util
import tempfile
import unittest
from pathlib import Path

import pytest

_HERE = Path(__file__).resolve().parent
_SPEC = importlib.util.spec_from_file_location(
    "leaves_no_artifacts_under_test", _HERE / "test_fail_open_mutation_leaves_no_artifacts.py"
)
lna = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(lna)

OPEN = pytest.mark.xfail(
    strict=True,
    reason="RED: leaves_no_artifacts stub signature lacks test_args; the stub never runs",
)


class LeavesNoArtifactsStubIsInvoked(unittest.TestCase):
    @OPEN
    def test_drive_reaches_the_stub_instead_of_raising_typeerror(self):
        case = lna.MutationRunArtifacts()
        with tempfile.TemporaryDirectory() as tmp:
            repo = lna._make_repo(tmp)
            _code, output = case._drive(repo)
        self.assertNotIn(
            "unexpected keyword argument",
            output,
            f"the stub was never called (call-shape TypeError); output:\n{output}",
        )


if __name__ == "__main__":
    unittest.main()
