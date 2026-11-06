#!/usr/bin/env python3
"""Audit B item 18 (memory: fail-open-review-checklist §3, formerly
forwarding-arms-have-zero-kill-rate).

A forwarding `Determination::Undetermined` arm reverted to `=> {}` compiles
and, measured 2026-08-06 (propguard 9a2eb9f0, git.rs:273), was killed by ZERO
tests. The memory's mechanization plan: run `scripts/check-fail-open-mutation.py`
per changed Undetermined arm in pre-push. Today nothing runs it at all.

Property asserted: `.githooks/pre-push` has an executable (non-comment) line
that invokes `check-fail-open-mutation.py`.

NOT asserted here (stated, not hidden): that the invocation is scoped to the
arms the pushed range changed. That needs an interface the fix will define;
its implementer's tests must pin it. This test only fails while the script is
unwired.

Control: the script exists, and pre-push does wire other scripts on executable
lines, so the line scan below is able to see a wiring when one exists.
"""
import re
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
PRE_PUSH = REPO / ".githooks" / "pre-push"
SCRIPT = "check-fail-open-mutation.py"


def _code_lines(text):
    return [ln for ln in text.splitlines() if ln.strip() and not ln.lstrip().startswith("#")]


class MutationHarnessWiredInPrePush(unittest.TestCase):
    def test_control_scanner_sees_existing_wiring(self):
        self.assertTrue((REPO / "scripts" / SCRIPT).is_file())
        lines = _code_lines(PRE_PUSH.read_text(encoding="utf-8"))
        self.assertTrue(any("check-plugin-rollout.py" in ln for ln in lines))

    @unittest.expectedFailure  # backlog 4f19b628: open defect
    def test_pre_push_runs_fail_open_mutation(self):
        lines = _code_lines(PRE_PUSH.read_text(encoding="utf-8"))
        hits = [ln for ln in lines if re.search(re.escape(SCRIPT), ln)]
        self.assertTrue(
            hits,
            f".githooks/pre-push never executes scripts/{SCRIPT}; forwarding "
            "Undetermined arms stay unmeasured (zero-kill arms ship silently)",
        )


if __name__ == "__main__":
    unittest.main()
