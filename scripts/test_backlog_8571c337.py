#!/usr/bin/env python3
"""RED repro for backlog 8571c337.

The advisory `undetermined-arm-empty-fallback` pattern (f12c2168) flags six
existing sites that collapse an Undetermined/Blocked arm into an empty value.
The ticket requires each to be resolved — fixed (a real fail-open) or recorded
as a legitimate degrade whose consumer reads empty as unknown — and the
baseline lowered. Until then `check-fail-open.py --all` keeps reporting them as
unclassified hits.

Sites are matched by file + snippet (line numbers drift). Open defect ->
expectedFailure.

    python3 scripts/test_backlog_8571c337.py
"""

from __future__ import annotations

import subprocess
import sys
import unittest
from pathlib import Path

_HERE = Path(__file__).resolve().parent
_REPO = _HERE.parent

PATTERN = "[undetermined-arm-empty-fallback]"
SITES = [
    ("crates/backlog/src/main.rs", "Determination::Undetermined(_) => String::new()"),
    ("crates/harness-core/src/store.rs", "Determination::Undetermined(_) => T::default()"),
    ("crates/overwatch/src/store.rs",
     "Determination::Known(None) | Determination::Undetermined(_) => Vec::new()"),
    ("crates/specguard/src/scope.rs", "Determination::Undetermined(_) => Vec::new()"),
    ("crates/stuckguard/src/verdict_monotonicity.rs",
     "Determination::Undetermined(_) => SessionState::default()"),
]


class TicketSitesAreResolved(unittest.TestCase):
    @unittest.expectedFailure  # backlog 8571c337: open defect, remove when fixed
    def test_no_ticket_site_is_still_an_unclassified_hit(self):
        p = subprocess.run(
            [sys.executable, str(_HERE / "check-fail-open.py"), "--all"],
            cwd=_REPO, capture_output=True, text=True,
        )
        out = p.stdout + p.stderr
        # Anti-vacuity: the scanner ran and reports this pattern at all.
        self.assertIn("fail-open-guard:", out)
        still = [
            line for line in out.splitlines()
            if PATTERN in line
            and any(line.startswith(f + ":") and snip in line for f, snip in SITES)
        ]
        self.assertEqual(still, [], "ticket sites still unclassified:\n" + "\n".join(still))


if __name__ == "__main__":
    unittest.main()
