#!/usr/bin/env python3
"""Repro for backlog 881d7933 / b0cacd15 site 2.

check-fail-open.py must flag `std::fs::read_dir(dir).map(..).unwrap_or_default()`
in crates/harness-status/src/path_shadow.rs::list_binary_names (read_dir is 7
lines above the unwrap, outside READDIR_WINDOW=6).

Run: python3 scripts/test_backlog_881d7933.py
"""
from __future__ import annotations

import importlib.util
import unittest
from pathlib import Path

_HERE = Path(__file__).resolve().parent
_SPEC = importlib.util.spec_from_file_location("check_fail_open", _HERE / "check-fail-open.py")
fo = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(fo)

REPO = _HERE.parent


class Backlog881d7933(unittest.TestCase):
    @unittest.expectedFailure  # backlog 881d7933: open defect, remove expectedFailure when fixed
    def test_path_shadow_list_binary_names_is_detected(self):
        p = REPO / "crates/harness-status/src/path_shadow.rs"
        hits = fo.scan_file(p)
        flagged = [h for h in hits if 99 <= h[0] <= 108]
        self.assertTrue(flagged, f"list_binary_names (path_shadow.rs:99-108) not flagged; hits={hits}")

    def test_site1_plugins_dir_nonempty_is_detected_control(self):
        p = REPO / "crates/harness-status/src/plugins.rs"
        hits = fo.scan_file(p)
        self.assertTrue([h for h in hits if 106 <= h[0] <= 110], f"hits={hits}")


if __name__ == "__main__":
    unittest.main()
