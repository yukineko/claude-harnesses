#!/usr/bin/env python3
"""backlog 65dfc23e: session-rollout-drift.py tells the operator to fix an
ENABLEMENT finding (checker rc=2) with `scripts/rollout-plugins.sh`, but the
checker it mirrors says the opposite: scripts/check-plugin-rollout.py documents
that "`rollout-plugins.sh` does NOT enable plugins" and that rc 2 is fixed by
editing settings.json and restarting Claude Code. Running the named remedy
leaves the finding exactly where it was.

The test drives the real `classify` with rc=2 and an EMPTY checker body, so any
mention of rollout-plugins.sh in the notice comes from this script's own remedy
text, not from echoed checker output.

Repro:  python3 -m pytest --runxfail scripts/test_backlog_65dfc23e.py
"""

from __future__ import annotations

import importlib.util
import os
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
MODPATH = os.path.join(HERE, "session-rollout-drift.py")
CHECKER = os.path.join(HERE, "check-plugin-rollout.py")

_spec = importlib.util.spec_from_file_location("session_rollout_drift", MODPATH)
mod = importlib.util.module_from_spec(_spec)
exec(  # noqa: S102
    compile(_spec.loader.get_source(_spec.name), _spec.origin, "exec", dont_inherit=True),
    mod.__dict__,
)


class EnablementRemedyIsNotRollout(unittest.TestCase):
    def test_precondition_checker_says_rollout_does_not_enable(self):
        # Anchors the claim to the checker's own words; if this ever stops
        # holding, the property below needs re-deciding, not this test.
        with open(CHECKER, encoding="utf-8") as f:
            self.assertIn("`rollout-plugins.sh` does NOT enable plugins", f.read())
        self.assertEqual(mod.RC_ENABLEMENT, 2)

    @unittest.expectedFailure
    def test_enablement_notice_does_not_prescribe_rollout(self):
        n = mod.classify(mod.RC_ENABLEMENT, "")
        self.assertIsNotNone(n, "rc=2 must still be reported")
        self.assertNotIn(
            "rollout-plugins.sh",
            n,
            "rc=2 (a GATE crate not enabled / settings.json absent) is not fixed "
            "by rollout-plugins.sh, which does NOT enable plugins; the notice "
            "prescribes a remedy that cannot clear it",
        )


if __name__ == "__main__":
    unittest.main(verbosity=1)
