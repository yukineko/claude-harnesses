#!/usr/bin/env python3
"""Closure-audit regression tests for scripts/check-fail-open.py (audit batch b1_1,
2026-10-02, written by the independent closure verifier).

* backlog f12c2168 — after 836a1aa3 renamed 47 sites from `Err(..)` to
  `Required::Blocked(..)`, the scanner was blind to an empty/default fallback in
  a `Required::Blocked` / `Undetermined` arm. GREEN at HEAD (c7d17849 added
  RS_UNDET_ARM_EMPTY). Note: the RED half (scanner without the fix) was NOT
  observed by this verifier — see the audit result line.
* backlog b0cacd15 (closed as DUPLICATE of 881d7933) — its completion condition
  names two sites; site 2, crates/harness-status/src/path_shadow.rs
  `list_binary_names` (`read_dir(..)` ... `.unwrap_or_default()` 7 lines apart),
  is still not detected (READDIR_WINDOW = 6). Skipped = RED today; un-skip when
  881d7933 lands.
"""
import importlib.util
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
_spec = importlib.util.spec_from_file_location("check_fail_open", ROOT / "scripts" / "check-fail-open.py")
cfo = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(cfo)


def kinds(lines):
    return [k for (_ln, k, _txt) in cfo.scan_rust(lines)]


class RequiredBlockedArm(unittest.TestCase):
    """f12c2168: the renamed spelling must be as visible as the old one."""

    def test_old_err_arm_spelling_is_flagged(self):
        src = [
            "fn f(r: Result<Vec<u8>, E>) -> Vec<u8> {",
            "    match r {",
            "        Ok(v) => v,",
            "        Err(_) => Vec::new(),",
            "    }",
            "}",
        ]
        self.assertTrue(kinds(src), "control: the Err-arm spelling must be flagged")

    def test_required_blocked_arm_empty_fallback_is_flagged(self):
        src = [
            "fn f(r: Required<Vec<u8>>) -> Vec<u8> {",
            "    match r {",
            "        Required::Known(v) => v,",
            "        Required::Blocked(_) => Vec::new(),",
            "    }",
            "}",
        ]
        self.assertTrue(
            kinds(src),
            "Required::Blocked(_) => Vec::new() is the same fail-open as Err(_) => Vec::new() "
            "and must not scan clean (backlog f12c2168)",
        )

    def test_required_blocked_arm_default_fallback_is_flagged(self):
        src = [
            "fn f(r: Required<Cfg>) -> Cfg {",
            "    match r {",
            "        Required::Known(v) => v,",
            "        Required::Blocked(_) => Default::default(),",
            "    }",
            "}",
        ]
        self.assertTrue(kinds(src), "Required::Blocked(_) => Default::default() must be flagged")


class PathShadowSite2(unittest.TestCase):
    """b0cacd15 / 881d7933: the second named site."""

    @unittest.skip(
        "backlog 881d7933 / b0cacd15 OPEN: path_shadow.rs list_binary_names "
        "(read_dir .. unwrap_or_default, 7 lines apart) is not detected"
    )
    def test_path_shadow_list_binary_names_is_detected(self):
        p = ROOT / "crates" / "harness-status" / "src" / "path_shadow.rs"
        text = p.read_text()
        self.assertIn("fn list_binary_names", text, "fixture site moved; re-anchor this test")
        hits = cfo.scan_file(p)
        lines = text.splitlines()
        start = next(i for i, l in enumerate(lines) if "fn list_binary_names" in l)
        end = start + 12
        in_fn = [h for h in hits if start < h[0] <= end + 1]
        self.assertTrue(in_fn, f"no check-fail-open hit inside list_binary_names; all hits: {hits}")


if __name__ == "__main__":
    unittest.main()
