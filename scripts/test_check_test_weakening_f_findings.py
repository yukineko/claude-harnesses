#!/usr/bin/env python3
"""RED repros for check-test-weakening.py (file-script-findings F-F, F-G).

F-F: cfg_test_modules() runs CFG_TEST_RE.finditer over the RAW source and, for
a `#[cfg(test)]` that is not a gated `mod x;`, takes `src.find("{", m.end())`.
  (1) `#[cfg(test)]` text inside a comment is treated as an attribute;
  (2) `#[cfg(test)]` text inside a string literal likewise;
  (3) `#[cfg(test)] use x;` (an attribute on a non-module item) grabs the next
      unrelated block.
In each case the PRODUCTION fn that follows is classified as test surface, so a
production `assert!` removal is reported as a test weakening. Expected: clean.

F-G: a `#[cfg(test)] mod tests;` declared inside an inline `mod a { ... }` block
is not found by test_body_root (documented limitation); when only the body file
src/a/tests.rs changes it is scanned as PRODUCTION code, so dropping an
assertion there is clean (exit 0). Expected: weakened (1) or undetermined (2),
never clean.

    python3 scripts/test_check_test_weakening_f_findings.py
"""
from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from test_check_test_weakening import GateTestCase  # noqa: E402

LIB = "crates/demo/src/lib.rs"

PROD_TWO = "pub fn prod(n: i32) -> i32 {\n    assert!(n < 1000);\n    assert!(n >= 0);\n    n\n}\n"
PROD_ONE = "pub fn prod(n: i32) -> i32 {\n    assert!(n < 1000);\n    n\n}\n"


class CfgTestTextIsNotAnAttribute(GateTestCase):
    def _prod_assert_dropped(self, prefix: str):
        repo = self.make_repo({LIB: prefix + PROD_TWO})
        repo.write(LIB, prefix + PROD_ONE)
        repo.commit("refactor: production fn loses a runtime assert!")
        return self.run_gate(repo.root)

    def test_cfg_test_in_comment_is_not_an_attribute(self):
        rc, out, err = self._prod_assert_dropped("// see #[cfg(test)] below\n")
        self.assertEqual(rc, 0, f"production code flagged via comment text\nSTDOUT:{out}\nSTDERR:{err}")

    def test_cfg_test_in_string_literal_is_not_an_attribute(self):
        rc, out, err = self._prod_assert_dropped('pub const DOC: &str = "#[cfg(test)]";\n')
        self.assertEqual(rc, 0, f"production code flagged via string text\nSTDOUT:{out}\nSTDERR:{err}")

    def test_cfg_test_use_does_not_capture_the_next_block(self):
        rc, out, err = self._prod_assert_dropped("#[cfg(test)]\nuse std::fmt::Debug;\n")
        self.assertEqual(rc, 0, f"`#[cfg(test)] use` captured the next fn body\nSTDOUT:{out}\nSTDERR:{err}")


A_DECLARER = "#[allow(dead_code)]\nmod a {\n    #[cfg(test)]\n    mod tests;\n}\n"
A_BODY_TWO = "use super::*;\n#[test]\nfn t() {\n    assert_eq!(1, 1);\n    assert_eq!(2, 2);\n}\n"
A_BODY_ONE = "use super::*;\n#[test]\nfn t() {\n    assert_eq!(1, 1);\n}\n"


class UnreachedDeclarer(GateTestCase):
    def test_body_file_behind_inline_mod_declarer_is_not_clean(self):
        body = "crates/demo/src/a/tests.rs"
        repo = self.make_repo({LIB: A_DECLARER, body: A_BODY_TWO})
        repo.write(body, A_BODY_ONE)
        repo.commit("test: weaken a/tests.rs only")
        rc, out, err = self.run_gate(repo.root)
        self.assertIn(rc, (1, 2), f"weakened test body scanned as production: clean\nSTDOUT:{out}\nSTDERR:{err}")


if __name__ == "__main__":
    unittest.main(verbosity=2)
