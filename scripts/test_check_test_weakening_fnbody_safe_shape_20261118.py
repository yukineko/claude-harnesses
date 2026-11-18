#!/usr/bin/env python3
"""RED repro (2026-11-18, user ruling): check-test-weakening must accept a
proven-safe shape for a Rust integration-test file that holds a
`macro_rules!` test template -- including a template whose matcher uses a
repetition such as `($(#[$attr:meta])* $name:ident, $prog:expr) => {...}` --
when the change edits lines ONLY inside ordinary fn bodies (or impl fn
bodies) that lie outside every `macro_rules!` template. Every template body
and every macro invocation stays byte-identical, and no `#[test]` item is
removed or weakened.

Before the shape landed (scripts/weakening_macro_fnbody.py) the gate raised
Undetermined (exit 2) for this, verbatim:
"#[test] inside a macro_rules! template in a file that exists at the base --
the change is not one of the proven-safe shapes". Concrete trigger: GH #1697,
crates/blastguard/tests/scope_redirect_relaxation_boundaries_20261108.rs,
where a `format!("/private/tmp/...")` literal inside `Fx::new`,
`impl Drop for Fx::drop` and a `#[test] fn` body is replaced by a helper call.

RED (marked `@unittest.expectedFailure` until the shape landed; each asserts
exit 0):
  a. one line in a helper fn body changes (literal -> fn call);
  b. the same inside an `impl Drop` fn body;
  c. the same inside a hand-written `#[test] fn` body (asserts kept);
  d. the #1697 shape: (a)+(b)+(c) plus an inserted `#[path=...] mod x;`
     after the inner `#![allow(...)]` attribute (among the existing
     `#[path] mod` items) and edited `//!` / `///` doc comment lines.

CONTROLS (non-zero today, must stay non-zero after the shape lands). Each
one carries a safe helper-fn-body edit as well, so the new shape cannot
accept it by ignoring the unsafe part:
  template body edit; removed invocation; an invocation's arguments edited
  inside a fn body that holds it; deleted #[test] fn; assert! removed from a
  #[test] fn body; #[ignore] added; a line moved from one fn to another; a
  new macro invocation added inside an edited fn body.

Repro: python3 -m pytest scripts/test_check_test_weakening_fnbody_safe_shape_20261118.py -q
"""
from __future__ import annotations

import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent / "check-test-weakening.py"

BASE = """\
#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Boundary tests for the scope relaxation (fixture-shape repro).
//!
//! Fixture: a fake scratchpad under /private/tmp.

#[path = "support/bounded.rs"]
mod bounded;

use std::path::PathBuf;

struct Fx {
    dir: PathBuf,
    uniq: String,
}

fn tmp_root() -> String {
    String::from("/tmp")
}

impl Fx {
    fn new(name: &str) -> Fx {
        let uniq = format!("{}-{name}", std::process::id());
        let dir = PathBuf::from(format!("/private/tmp/bg-fx-{uniq}"));
        Fx { dir, uniq }
    }
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(format!("/private/tmp/bg-fx-{}", self.uniq));
    }
}

/// Runs `prog` against a scratch path under /private/tmp.
fn run(prog: &str) -> String {
    assert!(!prog.is_empty());
    let p = format!("/private/tmp/run-{prog}");
    p
}

fn nested_cases() {
    py_hostile!(nested_t, "z");
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
py_hostile!(hostile_three, "c");

#[test]
fn standalone_control_allows() {
    let fx = Fx::new("s");
    let p = format!("/private/tmp/standalone");
    assert!(fx.dir.to_str().unwrap().starts_with("/private"));
    assert_eq!(p.len(), 22);
}
"""

# Safe single-line fn-body edits (literal -> helper call), one per fn kind.
HELPER_OLD = '    let p = format!("/private/tmp/run-{prog}");\n'
HELPER_NEW = '    let p = format!("{}/run-{prog}", tmp_root());\n'
NEW_OLD = '        let dir = PathBuf::from(format!("/private/tmp/bg-fx-{uniq}"));\n'
NEW_NEW = '        let dir = PathBuf::from(format!("{}/bg-fx-{uniq}", tmp_root()));\n'
DROP_OLD = '        let _ = std::fs::remove_dir_all(format!("/private/tmp/bg-fx-{}", self.uniq));\n'
DROP_NEW = '        let _ = std::fs::remove_dir_all(format!("{}/bg-fx-{}", tmp_root(), self.uniq));\n'
TEST_OLD = '    let p = format!("/private/tmp/standalone");\n'
TEST_NEW = '    let p = format!("{}/standalone", tmp_root());\n'

MOD_ANCHOR = '#[path = "support/bounded.rs"]\nmod bounded;\n'
MOD_INSERT = '\n#[path = "support/tmp_root.rs"]\nmod tmp_root_support;\n'
INNER_DOC_OLD = "//! Fixture: a fake scratchpad under /private/tmp.\n"
INNER_DOC_NEW = "//! Fixture: a fake scratchpad under the canonical temp root.\n"
OUTER_DOC_OLD = "/// Runs `prog` against a scratch path under /private/tmp.\n"
OUTER_DOC_NEW = "/// Runs `prog` against a scratch path under the canonical temp root.\n"


def edit(src: str, *pairs: tuple[str, str]) -> str:
    """Apply each (old, new) replacement exactly once; refuse a vacuous edit."""
    for old, new in pairs:
        if src.count(old) != 1:
            raise AssertionError(f"anti-vacuity: {old!r} occurs {src.count(old)}x")
        src = src.replace(old, new)
    return src


class FnBodySafeShape(unittest.TestCase):
    def setUp(self) -> None:
        td = tempfile.TemporaryDirectory(prefix="twfnbody.")
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
        self.file = self.repo / "crates" / "c" / "tests" / "fnbody_repro.rs"

    def git(self, *args: str) -> str:
        return subprocess.run(["git", *args], cwd=self.repo, env=self.env, check=True,
                              capture_output=True, text=True).stdout

    def base(self, content: str = BASE) -> None:
        self.assertIn("$(#[$attr:meta])*", content, "anti-vacuity: repetition matcher")
        self.assertIn("py_hostile!(hostile_one", content, "anti-vacuity: invocations")
        self.file.write_text(content)
        self.git("add", "-A")
        self.git("commit", "-q", "-m", "C0")
        self.git("checkout", "-q", "-b", "feat")

    def change(self, content: str) -> None:
        self.assertNotEqual(content, BASE, "anti-vacuity: the file changes")
        self.file.write_text(content)
        self.git("add", "-A")
        self.assertIn("fnbody_repro.rs", self.git("diff", "--cached", "--name-only"),
                      "anti-vacuity: the change is staged")

    def scan(self) -> tuple[int, str]:
        msg = self.tmp / "COMMIT_EDITMSG"
        msg.write_text("use the canonical temp root in fixtures\n")
        r = subprocess.run([sys.executable, str(SCRIPT), "--repo", str(self.repo),
                            "--pending-msg", str(msg)],
                           cwd=self.repo, env=self.env, capture_output=True,
                           text=True, timeout=60)
        return r.returncode, r.stdout + r.stderr

    def assert_clean(self, after: str) -> None:
        self.base()
        self.change(after)
        rc, out = self.scan()
        self.assertEqual(rc, 0, "RED: fn-body-only edit outside every template "
                         "was not accepted: " + out)

    def assert_not_clean(self, after: str) -> None:
        self.base()
        self.change(after)
        rc, out = self.scan()
        self.assertIn(rc, (1, 2), "control must block (finding=1 / undetermined=2): "
                      f"rc={rc} " + out)

    # -- RED ---------------------------------------------------------------
    def test_red_a_helper_fn_body_literal_to_call_passes(self) -> None:
        self.assert_clean(edit(BASE, (HELPER_OLD, HELPER_NEW)))

    def test_red_b_impl_drop_fn_body_edit_passes(self) -> None:
        self.assert_clean(edit(BASE, (DROP_OLD, DROP_NEW)))

    def test_red_c_standalone_test_fn_body_edit_keeps_asserts_passes(self) -> None:
        after = edit(BASE, (TEST_OLD, TEST_NEW))
        self.assertEqual(after.count("assert"), BASE.count("assert"))
        self.assertEqual(after.count("#[test]"), BASE.count("#[test]"))
        self.assert_clean(after)

    def test_red_d_gh1697_shape_passes(self) -> None:
        after = edit(
            BASE,
            (NEW_OLD, NEW_NEW), (DROP_OLD, DROP_NEW), (TEST_OLD, TEST_NEW),
            (HELPER_OLD, HELPER_NEW),
            (MOD_ANCHOR, MOD_ANCHOR + MOD_INSERT),
            (INNER_DOC_OLD, INNER_DOC_NEW), (OUTER_DOC_OLD, OUTER_DOC_NEW),
        )
        self.assertLess(after.index("#![allow("), after.index("mod tmp_root_support;"))
        self.assert_clean(after)

    # -- CONTROLS (each also carries the safe helper-body edit) -------------
    def test_control_template_body_edit_blocks(self) -> None:
        self.assert_not_clean(edit(
            BASE, (HELPER_OLD, HELPER_NEW),
            ("            assert_eq!(out.len(), out.trim().len());\n",
             "            assert_eq!(out.len(), out.len());\n"),
        ))

    def test_control_removed_invocation_blocks(self) -> None:
        self.assert_not_clean(edit(
            BASE, (HELPER_OLD, HELPER_NEW),
            ('py_hostile!(hostile_three, "c");\n', ""),
        ))

    def test_control_invocation_args_edited_inside_fn_body_blocks(self) -> None:
        self.assert_not_clean(edit(
            BASE, (HELPER_OLD, HELPER_NEW),
            ('    py_hostile!(nested_t, "z");\n', '    py_hostile!(nested_t, "zz");\n'),
        ))

    def test_control_deleted_test_fn_blocks(self) -> None:
        start = BASE.index("#[test]\nfn standalone_control_allows()")
        self.assert_not_clean(edit(BASE[:start], (HELPER_OLD, HELPER_NEW)))

    def test_control_assert_removed_from_test_fn_blocks(self) -> None:
        self.assert_not_clean(edit(
            BASE, (HELPER_OLD, HELPER_NEW),
            ("    assert_eq!(p.len(), 22);\n", ""),
        ))

    def test_control_ignore_added_blocks(self) -> None:
        self.assert_not_clean(edit(
            BASE, (HELPER_OLD, HELPER_NEW),
            ("#[test]\nfn standalone_control_allows()",
             "#[test]\n#[ignore]\nfn standalone_control_allows()"),
        ))

    def test_control_line_moved_between_fns_blocks(self) -> None:
        # `run`'s guard (exercised by every generated test) moves into Fx::new.
        moved = "    assert!(!prog.is_empty());\n"
        self.assert_not_clean(edit(
            BASE, (HELPER_OLD, HELPER_NEW), (moved, ""),
            ("        Fx { dir, uniq }\n", "    " + moved + "        Fx { dir, uniq }\n"),
        ))

    def test_control_new_invocation_inside_edited_fn_body_blocks(self) -> None:
        self.assert_not_clean(edit(
            BASE,
            (HELPER_OLD, HELPER_NEW + '    py_hostile!(#[ignore] smuggled, "q");\n'),
        ))


if __name__ == "__main__":
    unittest.main()
