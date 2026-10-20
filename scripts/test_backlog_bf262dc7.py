#!/usr/bin/env python3
"""RED repro for backlog bf262dc7.

The workspace denies `unwrap_used` / `expect_used` / `panic` under
`[workspace.lints.clippy]`, but a crate only inherits that when its own
Cargo.toml opts in with a `[lints]` table. The ticket tracks the crates that
still do not opt in (28 at filing, 26 when re-measured on 7e635343), so clippy
never denies a production panic there.

Asserts every crate with a Cargo.toml carries a `[lints]` table. Open defect ->
expectedFailure.

    python3 scripts/test_backlog_bf262dc7.py
"""

from __future__ import annotations

import tomllib
import unittest
from pathlib import Path

_REPO = Path(__file__).resolve().parent.parent


class EveryCrateOptsIntoWorkspaceLints(unittest.TestCase):
    def _manifests(self):
        return sorted((_REPO / "crates").glob("*/Cargo.toml"))

    def test_manifests_found_control(self):
        self.assertGreater(len(self._manifests()), 10)

    @unittest.expectedFailure  # backlog bf262dc7: open defect, remove when fixed
    def test_every_crate_has_a_lints_table(self):
        missing = []
        for m in self._manifests():
            data = tomllib.loads(m.read_text())
            if "lints" not in data:
                missing.append(m.parent.name)
        self.assertEqual(missing, [], "%d crates without [lints]: %s"
                         % (len(missing), " ".join(missing)))


if __name__ == "__main__":
    unittest.main()
