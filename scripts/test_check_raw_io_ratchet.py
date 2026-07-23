#!/usr/bin/env python3
"""Unit tests for scripts/check-raw-io-ratchet.py (raw-io-ratchet).

Stdlib-only (`unittest`); loaded via importlib since the module under test has
a hyphen in its name (mirrors test_check_fail_open.py / test_check_hardcoded_secret.py).

Load-bearing properties pinned here:
  1. F->P on the receiver-aware regex: an earlier, receiver-blind version of
     RAW_IO_PATTERN (`\\bread_to_string\\s*\\(` with no `(?<!\\.)` lookbehind)
     over-counted the real repo by 4 — it matched `some_reader.read_to_string
     (&mut buf)` (the io::Read trait method, used for subprocess output capture
     in propguard/gate.rs:781, propguard/git.rs:96, specguard/main.rs:1074,
     specguard/forge/main.rs:557) as if it were the free function
     `std::fs::read_to_string(path)`. `test_method_call_is_not_flagged` pins
     the fix: it fails (RED) against the receiver-blind pattern and passes
     (GREEN) against the shipped one. Reproduce the historical RED directly:
       python3 -c "import re; p = re.compile(r'\\bread_to_string\\s*\\(');
       print(bool(p.search('so.read_to_string(&mut out)')))"   # -> True (wrong)
  2. `test_region_lines`/`scan_file` correctly exclude `#[cfg(test)]` regions
     and comments (test fixtures legitimately call these functions directly).
  3. `ratchet_verdict` is a pure function: at baseline -> 0, above -> 1
     (regression), below -> 1 (unlocked improvement, must re-pin).
  4. `read_baseline` fails closed (raises BaselineError, never defaults to 0)
     on a missing file, an empty file, a multi-line file, or a non-integer.
  5. End-to-end anti-vacuity control experiment (mirrors the `panic = "deny"`
     workspace-lint experiment recorded in the root Cargo.toml near c23ac506):
     with a synthetic `crates/<gate-crate>/src/*.rs` fixture at exactly the
     pinned baseline count, the gate passes (GREEN); adding ONE more raw-IO
     call trips it (exit 1) with NOTHING else in this repo's gate suite aware
     of that count (a live rerun of `check-fail-open.py`/`check-test-weakening.py`
     against the real repo with a temporarily-added extra call, done by hand
     during development, stayed exit 0 on both — see the implementation
     commit message for that raw transcript; not re-asserted here since it
     needs the real repo tree, not a synthetic fixture).
"""

from __future__ import annotations

import importlib.util
import tempfile
import unittest
from pathlib import Path

_HERE = Path(__file__).resolve().parent
_SPEC = importlib.util.spec_from_file_location(
    "check_raw_io_ratchet", _HERE / "check-raw-io-ratchet.py"
)
rio = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(rio)


class ScanFileDetectsFreeFunctionCalls(unittest.TestCase):
    def test_qualified_read_to_string_is_flagged(self):
        lines = ['let text = std::fs::read_to_string(&path);\n']
        self.assertEqual(len(rio.scan_file_lines(lines)), 1)

    def test_bare_read_dir_is_flagged(self):
        lines = ["use std::fs::read_dir;\n", "let entries = read_dir(dir)?;\n"]
        self.assertEqual(len(rio.scan_file_lines(lines)), 1)

    def test_command_new_is_flagged(self):
        lines = ['let c = Command::new("git");\n']
        self.assertEqual(len(rio.scan_file_lines(lines)), 1)

    def test_qualified_process_command_new_is_flagged(self):
        lines = ['let c = std::process::Command::new("git");\n']
        self.assertEqual(len(rio.scan_file_lines(lines)), 1)


class ReceiverAwareFalsePositiveDiscipline(unittest.TestCase):
    def test_method_call_is_not_flagged(self):
        """The io::Read trait method (`reader.read_to_string(&mut buf)`), used
        for subprocess stdout/stderr capture, is not the free function this
        gate polices. See module docstring point 1 for the historical
        over-count this regression-pins."""
        lines = ["    so.read_to_string(&mut out);\n"]
        self.assertEqual(rio.scan_file_lines(lines), [])

    def test_receiver_blind_pattern_would_have_flagged_the_method_call(self):
        """F->P proof: the naive pattern this gate's RAW_IO_PATTERN replaced
        DOES match the method call — demonstrating the lookbehind fix is not
        vacuous."""
        import re
        naive = re.compile(r"\bread_to_string\s*\(")
        self.assertTrue(naive.search("so.read_to_string(&mut out);"))

    def test_comment_line_is_not_flagged(self):
        lines = ["// std::fs::read_to_string(&path) explained here\n"]
        self.assertEqual(rio.scan_file_lines(lines), [])

    def test_cfg_test_region_is_not_flagged(self):
        lines = [
            "#[cfg(test)]\n",
            "mod tests {\n",
            "    fn fixture() {\n",
            "        let _ = std::fs::read_to_string(\"x\");\n",
            "    }\n",
            "}\n",
        ]
        self.assertEqual(rio.scan_file_lines(lines), [])

    def test_code_after_cfg_test_region_is_still_flagged(self):
        lines = [
            "#[cfg(test)]\n",
            "mod tests {\n",
            "    fn fixture() {}\n",
            "}\n",
            "fn production() {\n",
            "    let _ = std::fs::read_to_string(\"x\");\n",
            "}\n",
        ]
        self.assertEqual(len(rio.scan_file_lines(lines)), 1)


class RatchetVerdictIsPure(unittest.TestCase):
    def test_at_baseline_passes(self):
        code, _ = rio.ratchet_verdict(77, 77)
        self.assertEqual(code, 0)

    def test_above_baseline_is_a_regression(self):
        code, msg = rio.ratchet_verdict(78, 77)
        self.assertEqual(code, 1)
        self.assertIn("ROSE", msg)

    def test_below_baseline_is_an_unlocked_improvement(self):
        code, msg = rio.ratchet_verdict(76, 77)
        self.assertEqual(code, 1)
        self.assertIn("FELL", msg)


class ReadBaselineFailsClosed(unittest.TestCase):
    def test_missing_file_raises(self):
        with tempfile.TemporaryDirectory() as tmp:
            with self.assertRaises(rio.BaselineError):
                rio.read_baseline(Path(tmp) / "nope.baseline")

    def test_empty_file_raises(self):
        with tempfile.TemporaryDirectory() as tmp:
            p = Path(tmp) / "x.baseline"
            p.write_text("", encoding="utf-8")
            with self.assertRaises(rio.BaselineError):
                rio.read_baseline(p)

    def test_multi_line_file_raises(self):
        with tempfile.TemporaryDirectory() as tmp:
            p = Path(tmp) / "x.baseline"
            p.write_text("1\n2\n", encoding="utf-8")
            with self.assertRaises(rio.BaselineError):
                rio.read_baseline(p)

    def test_non_integer_raises(self):
        with tempfile.TemporaryDirectory() as tmp:
            p = Path(tmp) / "x.baseline"
            p.write_text("abc\n", encoding="utf-8")
            with self.assertRaises(rio.BaselineError):
                rio.read_baseline(p)

    def test_negative_raises(self):
        with tempfile.TemporaryDirectory() as tmp:
            p = Path(tmp) / "x.baseline"
            p.write_text("-1\n", encoding="utf-8")
            with self.assertRaises(rio.BaselineError):
                rio.read_baseline(p)

    def test_well_formed_baseline_reads(self):
        with tempfile.TemporaryDirectory() as tmp:
            p = Path(tmp) / "x.baseline"
            p.write_text("# comment\n77\n", encoding="utf-8")
            self.assertEqual(rio.read_baseline(p), 77)


class EndToEndAntiVacuityControlExperiment(unittest.TestCase):
    """Synthetic version of the RED (nothing catches an extra raw-IO call) ->
    GREEN (this gate catches it) experiment, using a throwaway fixture crate
    tree instead of the real repo (so this test does not depend on the real
    baseline count, which will drift over time)."""

    def _make_fixture(self, tmp: Path, extra_calls: int) -> None:
        src = tmp / "crates" / "blastguard" / "src"
        src.mkdir(parents=True)
        body = "pub fn f() {\n"
        for i in range(extra_calls):
            body += f'    let _ = std::fs::read_to_string("f{i}");\n'
        body += "}\n"
        (src / "lib.rs").write_text(body, encoding="utf-8")

    def test_at_pinned_baseline_passes(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp_path = Path(tmp)
            self._make_fixture(tmp_path, extra_calls=3)
            files = [tmp_path / "crates" / "blastguard" / "src" / "lib.rs"]
            count = sum(len(rio.scan_file(p)) for p in files)
            self.assertEqual(count, 3)
            code, _ = rio.ratchet_verdict(count, baseline=3)
            self.assertEqual(code, 0)

    def test_one_call_beyond_baseline_trips_the_ratchet(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp_path = Path(tmp)
            self._make_fixture(tmp_path, extra_calls=4)  # one more than "baseline" below
            files = [tmp_path / "crates" / "blastguard" / "src" / "lib.rs"]
            count = sum(len(rio.scan_file(p)) for p in files)
            self.assertEqual(count, 4)
            code, msg = rio.ratchet_verdict(count, baseline=3)
            self.assertEqual(code, 1)
            self.assertIn("ROSE", msg)


if __name__ == "__main__":
    unittest.main()
