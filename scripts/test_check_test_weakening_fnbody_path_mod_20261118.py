#!/usr/bin/env python3
"""Independent tests (2026-11-18) for the fn-body proven-safe shape of
check-test-weakening (scripts/weakening_macro_fnbody.py, commit 32f94d161),
written by an agent that did not implement it.

Focus: the `#[path = "..."] mod x;` insertion branch (`_path_mod_insert_ok`)
which none of the earlier tests reach after an inner `#![...]` attribute, and
the guards of the fn-body / doc-line branches.

  accept (exit 0):
    1. `#[path = "support/x.rs"] mod x;` inserted right after the inner
       `#![allow(...)]` and the `//!` doc block, plus a fn-body edit (GH #1697).
  reject (exit 1 or 2), each carrying a safe helper-body edit as well:
    2. the same insert between `#[test]` and `fn` (after an outer attribute);
       2b. the same after a non-test outer attribute (`#[inline]`);
    3. the insert under a module name that already appears in the base code;
    4. a fn-body edit adding `#[cfg(...)]` / `return` / `macro_rules!`;
    5. an edited `///` line that is really inside a raw string;
    6. a helper-fn edit that removes an `assert!` from the helper;
       6b. the same with the assert replaced so the assert count is unchanged.

Mutation runs: TW_GATE_SCRIPT may point the tests at a COPY of
check-test-weakening.py (in a directory holding mutated copies of its sibling
modules; the gate puts its own directory first on sys.path).

Repro: python3 -m pytest scripts/test_check_test_weakening_fnbody_path_mod_20261118.py -q
"""
from __future__ import annotations

import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(os.environ.get("TW_GATE_SCRIPT")
              or Path(__file__).resolve().parent / "check-test-weakening.py")

BASE = """\
#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Path-mod insertion tests (fixture-shape repro).
//!
//! Fixture: a fake scratchpad under /private/tmp.

use std::path::PathBuf;

const BANNER: &str = r#"
/// banner line kept verbatim
"#;

#[inline]
fn tmp_root() -> String {
    String::from("/tmp")
}

/// Runs `prog` against a scratch path under /private/tmp.
fn run(prog: &str) -> String {
    assert!(!prog.is_empty());
    let p = format!("/private/tmp/run-{prog}");
    p
}

macro_rules! py_hostile {
    ($(#[$attr:meta])* $name:ident, $prog:expr) => {
        $(#[$attr])*
        #[test]
        fn $name() {
            let out = run($prog);
            assert!(!out.is_empty());
            assert_eq!(out.len(), out.trim().len());
        }
    };
}

py_hostile!(hostile_one, "a");
py_hostile!(
    #[cfg(unix)]
    hostile_two,
    "b"
);

#[test]
fn standalone_control_allows() {
    let dir = PathBuf::from("/private/tmp/standalone");
    let p = format!("/private/tmp/standalone");
    assert!(dir.to_str().unwrap().starts_with("/private"));
    assert_eq!(p.len(), 22);
    assert!(!BANNER.is_empty());
}
"""

HELPER_OLD = '    let p = format!("/private/tmp/run-{prog}");\n'
HELPER_NEW = '    let p = format!("{}/run-{prog}", tmp_root());\n'
DOC_END = "//! Fixture: a fake scratchpad under /private/tmp.\n"
PATH_MOD_X = '#[path = "support/x.rs"] mod x;\n'
TEST_HEAD = "#[test]\nfn standalone_control_allows()"
KEPT_ASSERT = "    assert_eq!(p.len(), 22);\n"


def edit(src: str, *pairs: tuple[str, str]) -> str:
    """Apply each (old, new) replacement exactly once; refuse a vacuous edit."""
    for old, new in pairs:
        if src.count(old) != 1:
            raise AssertionError(f"anti-vacuity: {old!r} occurs {src.count(old)}x")
        src = src.replace(old, new)
    return src


class FnBodyPathModShape(unittest.TestCase):
    def setUp(self) -> None:
        td = tempfile.TemporaryDirectory(prefix="twpathmod.")
        self.addCleanup(td.cleanup)
        self.tmp = Path(os.path.realpath(td.name))
        home = self.tmp / "home"
        home.mkdir()
        self.env = {
            "GIT_AUTHOR_NAME": "t", "GIT_AUTHOR_EMAIL": "t@example.invalid",
            "GIT_COMMITTER_NAME": "t", "GIT_COMMITTER_EMAIL": "t@example.invalid",
            "GIT_CONFIG_GLOBAL": os.devnull, "GIT_CONFIG_NOSYSTEM": "1",
            "HOME": str(home), "TMPDIR": str(self.tmp), "LC_ALL": "C",
            "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
        }
        self.repo = self.tmp / "repo"
        (self.repo / "crates" / "c" / "tests").mkdir(parents=True)
        self.git("init", "-q", "-b", "main")
        self.file = self.repo / "crates" / "c" / "tests" / "pathmod_repro.rs"

    def git(self, *args: str) -> str:
        return subprocess.run(["git", *args], cwd=self.repo, env=self.env, check=True,
                              capture_output=True, text=True).stdout

    def run_gate(self, after: str) -> tuple[int, str]:
        self.assertIn("$(#[$attr:meta])*", BASE, "anti-vacuity: repetition matcher")
        self.file.write_text(BASE)
        self.git("add", "-A")
        self.git("commit", "-q", "-m", "C0")
        self.git("checkout", "-q", "-b", "feat")
        self.assertNotEqual(after, BASE, "anti-vacuity: the file changes")
        self.file.write_text(after)
        self.git("add", "-A")
        self.assertIn("pathmod_repro.rs", self.git("diff", "--cached", "--name-only"))
        msg = self.tmp / "COMMIT_EDITMSG"
        msg.write_text("use the canonical temp root in fixtures\n")
        r = subprocess.run([sys.executable, str(SCRIPT), "--repo", str(self.repo),
                            "--pending-msg", str(msg)],
                           cwd=self.repo, env=self.env, capture_output=True,
                           text=True, timeout=60)
        return r.returncode, r.stdout + r.stderr

    def assert_clean(self, after: str) -> None:
        rc, out = self.run_gate(after)
        self.assertEqual(rc, 0, f"proven-safe shape not accepted: rc={rc} " + out)

    def assert_not_clean(self, after: str) -> None:
        rc, out = self.run_gate(after)
        self.assertIn(rc, (1, 2), f"must block (finding=1 / undetermined=2): rc={rc} " + out)

    # -- 1. accept ----------------------------------------------------------
    def test_1_path_mod_after_inner_attr_and_doc_block_accepted(self) -> None:
        after = edit(BASE, (HELPER_OLD, HELPER_NEW), (DOC_END, DOC_END + PATH_MOD_X))
        lines = after.splitlines()
        # The insert directly follows the //! block, which follows #![allow(..)].
        k = lines.index(PATH_MOD_X.rstrip("\n"))
        self.assertTrue(all(ln.startswith("//!") for ln in lines[1:k]))
        self.assertTrue(lines[0].startswith("#![allow("))
        self.assert_clean(after)

    # -- 2. after an outer attribute ---------------------------------------
    def test_2_path_mod_between_test_attr_and_fn_rejected(self) -> None:
        self.assert_not_clean(edit(
            BASE, (HELPER_OLD, HELPER_NEW),
            (TEST_HEAD, "#[test]\n" + PATH_MOD_X + "fn standalone_control_allows()"),
        ))

    def test_2b_path_mod_after_non_test_outer_attr_rejected(self) -> None:
        # Isolates the item-boundary guard: with `#[test]` (test_2) the gate's
        # later #[test]->fn attribution also rejects; `#[inline]` has no such
        # backstop, so only `_at_item_boundary` stops the stolen attribute.
        self.assert_not_clean(edit(
            BASE, (HELPER_OLD, HELPER_NEW),
            ("#[inline]\nfn tmp_root()", "#[inline]\n" + PATH_MOD_X + "fn tmp_root()"),
        ))

    # -- 3. name already in the base ----------------------------------------
    def test_3_path_mod_name_seen_in_base_rejected(self) -> None:
        self.assertIn("tmp_root", BASE)
        self.assert_not_clean(edit(
            BASE, (HELPER_OLD, HELPER_NEW),
            (DOC_END, DOC_END + '#[path = "support/x.rs"] mod tmp_root;\n'),
        ))

    # -- 4. forbidden markers inside a fn-body edit -------------------------
    def test_4a_fn_body_edit_adds_cfg_attr_rejected(self) -> None:
        self.assert_not_clean(edit(
            BASE, (HELPER_OLD, HELPER_NEW),
            (KEPT_ASSERT, "    #[cfg(any())]\n" + KEPT_ASSERT),
        ))

    def test_4b_fn_body_edit_adds_return_rejected(self) -> None:
        self.assert_not_clean(edit(
            BASE, (HELPER_OLD, HELPER_NEW),
            (KEPT_ASSERT, "    return;\n" + KEPT_ASSERT),
        ))

    def test_4c_fn_body_edit_adds_macro_rules_rejected(self) -> None:
        self.assert_not_clean(edit(
            BASE, (HELPER_OLD, HELPER_NEW),
            (KEPT_ASSERT, "    macro_rules! noop { () => {}; }\n" + KEPT_ASSERT),
        ))

    # -- 5. `///` that is really inside a raw string -----------------------
    def test_5_doc_line_inside_raw_string_rejected(self) -> None:
        self.assert_not_clean(edit(
            BASE, (HELPER_OLD, HELPER_NEW),
            ("/// banner line kept verbatim\n", "/// banner line rewritten\n"),
        ))

    # -- 6. helper loses an assert! ----------------------------------------
    def test_6_helper_fn_edit_removes_assert_rejected(self) -> None:
        self.assert_not_clean(edit(
            BASE, (HELPER_OLD, HELPER_NEW),
            ("    assert!(!prog.is_empty());\n", ""),
        ))

    def test_6b_helper_fn_edit_replaces_assert_same_count_rejected(self) -> None:
        # Isolates the assert-survival guard: the assertion-macro count stays
        # the same, so the gate's count finding cannot catch the lost check.
        after = edit(
            BASE, (HELPER_OLD, HELPER_NEW),
            ("    assert!(!prog.is_empty());\n", "    assert!(prog.len() < usize::MAX);\n"),
        )
        self.assertEqual(after.count("assert"), BASE.count("assert"))
        self.assert_not_clean(after)


if __name__ == "__main__":
    unittest.main()
