#!/usr/bin/env python3
"""RED repro for backlog 04f58309 (hole 1 of 5b89f0f6).

`harness_core::verdict::Required` withholds `unwrap_or_default` / `is_ok` on
purpose, but an external extension trait can re-grant them — the type system
cannot close that (pinned by
crates/harness-core/tests/ui/verdict_known_holes/unsealed_paths.rs). No gate
detects it either: `check-fail-open.py`'s Required/Determination patterns are
arm-shaped, so an impl that erases via `if let` instead of a match arm is
invisible to the scanner (and thus to check-fail-open-diff.py, which reuses it).

Feeds the scanner a production-shaped source with such an impl and asserts it
is flagged. Open defect -> expectedFailure.

    python3 scripts/test_backlog_04f58309.py
"""

from __future__ import annotations

import importlib.util
import unittest
from pathlib import Path

_HERE = Path(__file__).resolve().parent
_SPEC = importlib.util.spec_from_file_location("check_fail_open", _HERE / "check-fail-open.py")
fo = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(fo)

REGRANT = """\
use harness_core::verdict::Required;

pub trait Erase<T> {
    fn unwrap_or_default(self) -> T;
    fn is_ok(&self) -> bool;
}

impl<T: Default> Erase<T> for Required<T> {
    fn unwrap_or_default(self) -> T {
        if let Required::Determined(v) = self { v } else { T::default() }
    }
    fn is_ok(&self) -> bool {
        matches!(self, Required::Determined(_))
    }
}
"""


class ExtensionTraitRegrantIsDetected(unittest.TestCase):
    @unittest.expectedFailure  # backlog 04f58309: open defect, remove when fixed
    def test_extension_trait_on_required_is_flagged(self):
        hits = fo.scan_rust(REGRANT.splitlines())
        self.assertTrue(
            hits, "an extension trait re-granting unwrap_or_default/is_ok on "
                  "Required produced no scanner finding",
        )

    def test_scanner_is_live_control(self):
        # Anti-vacuity: the same scanner does flag the arm-shaped erasure.
        hits = fo.scan_rust(["    Required::Blocked(_) => T::default(),"])
        self.assertTrue(hits, "control: arm-shaped erasure must be flagged")


if __name__ == "__main__":
    unittest.main()
