#!/usr/bin/env python3
"""RED repro (backlog 25b88447): check-test-weakening refuses (rc 2) the real blastguard fix to
crates/blastguard/tests/heredoc_body_gt_fp_20261107.rs, even after ed82c49b.

Observed 2026-11-08 at main 27834f97: the unstaged fix in
/Users/yuki/src/.harness-worktrees/fix-hdgt-regress2 (base = its HEAD blob,
after = its working-tree file, staged in a throwaway repo) gives

    check-test-weakening: undetermined — crates/blastguard/tests/heredoc_body_gt_fp_20261107.rs: #[test] inside a macro_rules! template in a file that exists at the base -- the change is not one of the proven-safe shapes (invocation counting included), so removed generated tests cannot be ruled out

The fix makes four kinds of change, none of which can remove or weaken an
existing test:
  (1) a top-level helper fn with a `///` doc comment, inserted right after
      `impl Fixture { .. }`;
  (2) `//` comment lines added to an existing comment above an invocation;
  (3) two invocation-argument edits;
  (4) a new top-level `#[test] fn control_..() { .. }` between two
      invocations.

Contract (same shapes as the real file; the file exists at the merge-base):
  RED      * the full combination (1)+(2)+(3)+(4) is clean (rc 0).
  CLEAN    * each change ALONE is clean, and these pass today: (1) via the
             invocation shape (`template_invocation_edit_is_safe`); (2) and
             (4) via the inserted-items shape (`_only_proven_safe_line_changes`);
             an edit to an existing comment's text between items via the
             invocation shape; (3) via the invocation shape (4f3da889). The
             combination fails because (4), an inserted `#[test] fn`, is
             accepted only by the inserted-items shape, and the invocation
             edits only by the other. Neither shape alone covers the whole
             change (probed by calling both predicates directly).
  KEEP     * an inserted `#[test] fn` carrying `#[ignore]` or
             `#[should_panic]`, or an inserted helper carrying `#[cfg(..)]`:
             rc 1 or 2 (pass today);
           * a comment edit that also changes code on the same line (part of a
             template body commented out): rc 2 (passes today);
           * a `/* */` block comment that swallows an invocation: rc 1 or 2
             (passes today).
  CLEAN PIN (coordinator correction of spec, 2026-11-08; not a weakening):
           * an inserted `#[test] fn` whose name duplicates an existing
             generated test name (`fn target_one`) is clean: a duplicate name
             is rustc E0428, a loud compile failure, so nothing is silently
             weakened;
           * an inserted `#[cfg(test)] mod` holding a test is clean: it only
             adds tests and cannot affect existing ones.
             The original spec asked for rc 2 here. That was a spec error, not
             a user ruling. Both are accepted by the inserted-items shape
             (`_only_proven_safe_line_changes`) and pinned at rc 0 so that a
             future tightening is a visible decision.

Set CTW_SCRIPT to run against another copy of the gate.

Repro:  python3 -m pytest --runxfail scripts/test_ctw_macro_rules_helper_comment_testfn.py
"""
from __future__ import annotations

import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(os.environ.get(
    "CTW_SCRIPT", str(Path(__file__).resolve().parent / "check-test-weakening.py")))

REL = "crates/demo/tests/heredoc_like.rs"

HEAD_PART = """\
struct Fixture;

impl Fixture {
    fn new(_n: &str) -> Fixture {
        Fixture
    }
    fn decision(&self, cmd: &str, _sub: bool) -> &'static str {
        if cmd.is_empty() { "deny" } else { "allow" }
    }
    fn assert_allow_both(&self, cmd: &str) {
        assert_eq!(self.decision(cmd, false), "allow", "{cmd}");
    }
}

"""

TEMPLATES_PART = """\
macro_rules! target {
    ($name:ident, $cmd:expr) => {
        #[test]
        fn $name() {
            Fixture::new(stringify!($name)).assert_allow_both($cmd);
        }
    };
}

macro_rules! control {
    ($name:ident, $check:ident, $cmd:expr) => {
        #[test]
        fn $name() {
            Fixture::new(stringify!($name)).$check($cmd);
        }
    };
}

"""

COMMENT_PART = """\
// ---- FP 1: git commit -F - heredoc after a cd whose target is a runtime value ----
// (a cd to a literal absolute dir with the same body is already Allow.)

"""

T1 = 'target!(\n    target_one,\n    "cd \\"$WT\\" && git add -A && git commit -q -F - <<\'EOF\'\\na -> b\\nEOF"\n);\n'
T1_EDIT = T1.replace("git add -A", "git add f.txt")
T2 = 'target!(\n    target_two,\n    "python3 - <<\'EOF\'\\nprint(3>2)\\nEOF"\n);\n\n'
C1 = 'control!(\n    control_one,\n    assert_allow_both,\n    "cd \\"$WT\\" && git commit -q -F - <<\'EOF\'\\na to b\\nEOF"\n);\n'
C1_EDIT = C1.replace("&& git commit", "&& git add f.txt && git commit")
C2 = 'control!(\n    control_two,\n    assert_allow_both,\n    "python3 - <<\'EOF\'\\nprint(3, 2)\\nEOF"\n);\n'

BASE = HEAD_PART + TEMPLATES_PART + COMMENT_PART + T1 + T2 + C1 + C2

# (1) helper with a `///` doc, right after `impl Fixture { .. }`.
HELPER = """\
/// Whole-tree staging must not be Allow for the main agent or a subagent,
/// whatever the heredoc body.
fn assert_restrictive_both(fixture: &Fixture, command: &str) {
    for sub in [false, true] {
        let d = fixture.decision(command, sub);
        assert_ne!(d, "allow", "`{command}` (subagent={sub}) must not be Allow");
    }
}

"""

# (2) `//` lines added to the existing comment above the first invocation.
COMMENT_ADDED = COMMENT_PART.replace(
    "already Allow.)\n",
    "already Allow.)\n//\n// The `git add` head stages an EXPLICIT path, so the `>` in the body\n"
    "// stays the only thing under test.\n", 1)
# An edit to an existing comment's text.
COMMENT_EDITED = COMMENT_PART.replace("is already Allow.", "is Allow today.", 1)

# (4) a new top-level #[test] fn between two invocations.
TEST_FN_USING_HELPER = """\
#[test]
fn control_cd_git_add_all_not_allow() {
    assert_restrictive_both(
        &Fixture::new("control_cd_git_add_all_not_allow"),
        "cd \\"$WT\\" && git add -A && git commit -q -F - <<'EOF'\\na to b\\nEOF",
    );
}
"""
TEST_FN_PLAIN = """\
#[test]
fn control_cd_git_add_all_not_empty() {
    let f = Fixture::new("control_cd_git_add_all_not_empty");
    assert_eq!(f.decision("cd x && git add -A", false), "allow");
}
"""


def build(helper: str = "", comment: str = COMMENT_PART, t1: str = T1, c1: str = C1,
          between: str = "", templates: str = TEMPLATES_PART) -> str:
    return HEAD_PART + helper + templates + comment + t1 + T2 + c1 + between + C2


FULL = build(HELPER, COMMENT_ADDED, T1_EDIT, C1_EDIT, TEST_FN_USING_HELPER)


class HelperCommentTestFn(unittest.TestCase):
    def setUp(self) -> None:
        td = tempfile.TemporaryDirectory(prefix="ctwhdgt.")
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
        (self.repo / Path(REL).parent).mkdir(parents=True)
        self.git("init", "-q", "-b", "main")
        (self.repo / REL).write_text(BASE)
        self.git("add", "-A")
        self.git("-c", "core.hooksPath=/dev/null", "-c", "commit.gpgsign=false",
                 "commit", "-q", "-m", "C0 on main")
        self.git("checkout", "-q", "-b", "feat")

    def git(self, *args: str) -> str:
        return subprocess.run(["git", *args], cwd=self.repo, env=self.env, check=True,
                              capture_output=True, text=True).stdout

    def scan(self, after_src: str) -> tuple[int, str]:
        self.assertNotEqual(after_src, BASE, "anti-vacuity: the change is a change")
        (self.repo / REL).write_text(after_src)
        self.git("add", "-A")  # staged and in the worktree: index == worktree
        msg = self.tmp / "COMMIT_EDITMSG"
        msg.write_text("commit\n")
        r = subprocess.run([sys.executable, str(SCRIPT), "--repo", str(self.repo),
                            "--pending-msg", str(msg)],
                           cwd=self.repo, env=self.env, capture_output=True,
                           text=True, timeout=60)
        return r.returncode, f"rc={r.returncode} stdout={r.stdout!r} stderr={r.stderr!r}"

    def assert_clean(self, src: str) -> None:
        rc, out = self.scan(src)
        self.assertNotIn("undetermined", out, "must yield a verdict, not Undetermined")
        self.assertEqual(rc, 0, out)

    def assert_rc_in(self, src: str, allowed: tuple) -> None:
        rc, out = self.scan(src)
        self.assertIn(rc, allowed, out)

    # -- anti-vacuity ----------------------------------------------------------

    def test_fixture_goes_down_the_template_path(self) -> None:
        # Removing an invocation is refused with the template reason today.
        rc, out = self.scan(BASE.replace(T2, "", 1))
        self.assertNotEqual(rc, 0, out)

    # -- CLEAN: each change alone (passes today) --------------------------------

    def test_helper_with_doc_comment_after_impl_is_clean(self) -> None:
        self.assert_clean(build(helper=HELPER))

    def test_comment_lines_added_between_items_is_clean(self) -> None:
        self.assert_clean(build(comment=COMMENT_ADDED))

    def test_comment_text_edited_between_items_is_clean(self) -> None:
        self.assert_clean(build(comment=COMMENT_EDITED))

    def test_new_plain_test_fn_between_invocations_is_clean(self) -> None:
        self.assert_clean(build(between=TEST_FN_PLAIN))

    # -- RED: the full combination (the real file's shape) --------------------

    @unittest.expectedFailure  # backlog 25b88447: open defect
    def test_full_combination_real_shape_is_clean(self) -> None:
        self.assert_clean(FULL)

    # -- CONTROL: (3) alone ------------------------------------------------------

    def test_control_invocation_arg_edits_alone_are_clean(self) -> None:
        self.assert_clean(build(t1=T1_EDIT, c1=C1_EDIT))

    # -- KEEP: must stay non-clean ---------------------------------------------

    def test_inserted_test_fn_with_ignore_is_not_clean(self) -> None:
        src = build(between=TEST_FN_PLAIN.replace("#[test]\n", "#[test]\n#[ignore]\n", 1))
        self.assert_rc_in(src, (1, 2))

    def test_inserted_test_fn_with_should_panic_is_not_clean(self) -> None:
        src = build(between=TEST_FN_PLAIN.replace("#[test]\n", "#[test]\n#[should_panic]\n", 1))
        self.assert_rc_in(src, (1, 2))

    def test_inserted_helper_with_cfg_is_not_clean(self) -> None:
        src = build(helper=HELPER.replace("fn assert_restrictive_both",
                                          "#[cfg(any())]\nfn assert_restrictive_both", 1))
        self.assert_rc_in(src, (1, 2))

    def test_inserted_test_fn_duplicating_generated_name_is_clean(self) -> None:
        """PIN rc 0 (coordinator correction of spec, 2026-11-08; not a
        weakening): a top-level `fn target_one` next to the generated
        `target_one` is rustc E0428, a loud compile failure, so no existing
        test can be silently weakened by it."""
        src = build(between=TEST_FN_PLAIN.replace(
            "fn control_cd_git_add_all_not_empty()", "fn target_one()", 1))
        self.assertIn("fn target_one()", src, "anti-vacuity: the duplicate is inserted")
        self.assert_clean(src)

    def test_comment_edit_that_changes_code_on_same_line_is_undetermined(self) -> None:
        templates = TEMPLATES_PART.replace(
            "            Fixture::new(stringify!($name)).assert_allow_both($cmd);\n",
            "            Fixture::new(stringify!($name)); // .assert_allow_both($cmd);\n", 1)
        self.assertNotEqual(templates, TEMPLATES_PART, "anti-vacuity")
        self.assert_rc_in(build(templates=templates), (2,))

    def test_block_comment_swallowing_an_invocation_is_not_clean(self) -> None:
        src = BASE.replace(T2, "/*\n" + T2 + "*/\n", 1)
        self.assert_rc_in(src, (1, 2))

    def test_inserted_mod_with_tests_is_clean(self) -> None:
        """PIN rc 0 (coordinator correction of spec, 2026-11-08; not a
        weakening): an inserted `#[cfg(test)] mod` is add-only. It brings new
        tests and cannot affect any existing one."""
        mod = ("#[cfg(test)]\nmod extra {\n    #[test]\n    fn inner() {\n"
               "        assert!(true);\n    }\n}\n")
        self.assert_clean(build(between=mod))


if __name__ == "__main__":
    unittest.main()
