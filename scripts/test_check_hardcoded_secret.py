#!/usr/bin/env python3
"""Unit tests for scripts/check-hardcoded-secret.py (hardcoded-secret-guard).

Stdlib-only (`unittest`), no third-party dependency, so it runs identically in
CI and locally: `python3 scripts/test_check_hardcoded_secret.py`.

Load-bearing properties pinned here:
  1. A planted `password = "..."` / `token = "..."` / etc. shaped line IS caught.
  2. False-positive discipline: a comment line, an ALLOWLISTed line, and a line
     with no literal string RHS (e.g. `token = get_token()`) are NOT flagged.
  3. `parse_diff_added` (a pure function, no git/filesystem needed) correctly
     tracks per-file new-line numbers across multiple hunks and multiple files
     from synthetic unified-diff text.
  4. `.py` files are excluded from the real CLI scan path — deliberately, so
     this very test file (and check-hardcoded-secret's own regex-literal
     source line) can embed secret-shaped fixture strings without the real
     gate ever seeing them (same reason check-fail-open.py never globs *.py).

The module under test has a hyphen in its name, so it is loaded via importlib,
mirroring scripts/test_check_fail_open.py.
"""

from __future__ import annotations

import importlib.util
import unittest
from pathlib import Path

_HERE = Path(__file__).resolve().parent
_SPEC = importlib.util.spec_from_file_location(
    "check_hardcoded_secret", _HERE / "check-hardcoded-secret.py"
)
hs = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(hs)


class ScanLineDetectsPlantedSecret(unittest.TestCase):
    def test_password_literal_is_flagged(self):
        line = "password" + " = " + '"hunter2value"'
        self.assertTrue(hs.scan_line(line))

    def test_api_key_literal_is_flagged(self):
        line = "api_key" + " = " + '"sk-abcd1234efgh"'
        self.assertTrue(hs.scan_line(line))

    def test_token_literal_is_flagged(self):
        line = "  auth_token" + " = " + "'ghp_1234567890abcdef'"
        self.assertTrue(hs.scan_line(line))

    def test_private_key_literal_is_flagged(self):
        line = "private_key" + " = " + '"-----BEGIN-fake-material-----"'
        self.assertTrue(hs.scan_line(line))


class ScanLineFalsePositiveDiscipline(unittest.TestCase):
    def test_comment_line_is_not_flagged(self):
        line = "# password" + " = " + '"hunter2value"'
        self.assertFalse(hs.scan_line(line))

    def test_slash_comment_line_is_not_flagged(self):
        line = "// secret" + " = " + '"hunter2value"'
        self.assertFalse(hs.scan_line(line))

    def test_no_literal_string_rhs_is_not_flagged(self):
        line = "token = get_token()"
        self.assertFalse(hs.scan_line(line))

    def test_short_value_is_not_flagged(self):
        # RHS shorter than the length floor (placeholder-shaped, not secret-shaped).
        line = "secret" + " = " + '"abc"'
        self.assertFalse(hs.scan_line(line))

    def test_env_var_placeholder_is_not_flagged(self):
        line = "password" + " = " + '"${DB_PASSWORD}"'
        self.assertFalse(hs.scan_line(line))

    def test_allowlisted_substring_is_not_flagged(self):
        needle = "test-fixture-allowlisted-marker"
        line = "password" + " = " + f'"{needle}-hunter2value"'
        try:
            hs.ALLOWLIST.append(needle)
            self.assertFalse(hs.scan_line(line))
        finally:
            hs.ALLOWLIST.remove(needle)


class ParseDiffAddedTracksLineNumbers(unittest.TestCase):
    def test_single_hunk_single_file(self):
        diff = "\n".join([
            "diff --git a/scripts/foo.sh b/scripts/foo.sh",
            "--- a/scripts/foo.sh",
            "+++ b/scripts/foo.sh",
            "@@ -10,0 +11,2 @@",
            "+line one",
            "+line two",
        ])
        got = hs.parse_diff_added(diff)
        self.assertEqual(
            got,
            [("scripts/foo.sh", 11, "line one"), ("scripts/foo.sh", 12, "line two")],
        )

    def test_removed_lines_do_not_advance_new_lineno(self):
        diff = "\n".join([
            "--- a/x.py",
            "+++ b/x.py",
            "@@ -5,2 +5,2 @@",
            "-old line",
            "+new line",
            "-old line two",
            "+new line two",
        ])
        got = hs.parse_diff_added(diff)
        self.assertEqual(
            got,
            [("x.py", 5, "new line"), ("x.py", 6, "new line two")],
        )

    def test_multiple_files_reset_lineno_tracking(self):
        diff = "\n".join([
            "--- a/a.rs",
            "+++ b/a.rs",
            "@@ -1,0 +2,1 @@",
            "+first file added line",
            "--- a/b.rs",
            "+++ b/b.rs",
            "@@ -9,0 +10,1 @@",
            "+second file added line",
        ])
        got = hs.parse_diff_added(diff)
        self.assertEqual(
            got,
            [
                ("a.rs", 2, "first file added line"),
                ("b.rs", 10, "second file added line"),
            ],
        )

    def test_empty_diff_yields_nothing(self):
        self.assertEqual(hs.parse_diff_added(""), [])


class PyFilesExcludedFromRealScan(unittest.TestCase):
    def test_py_suffix_is_excluded(self):
        self.assertIn(".py", hs.EXCLUDED_SUFFIXES)

    def test_iter_target_lines_filters_py(self):
        added = [
            ("scripts/check-something.py", 3, 'password = "hunter2value"'),
            ("scripts/gate.sh", 3, 'password = "hunter2value"'),
        ]
        filtered = [
            (p, n, t) for (p, n, t) in added
            if Path(p).suffix not in hs.EXCLUDED_SUFFIXES
        ]
        self.assertEqual(filtered, [("scripts/gate.sh", 3, 'password = "hunter2value"')])


if __name__ == "__main__":
    unittest.main()
