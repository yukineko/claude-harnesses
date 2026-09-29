// このファイルは丸ごと integration test なので expect/unwrap を許可する。
#![allow(clippy::expect_used)]
//! RED tests for backlog item 1875f626: `unknown_verb_protected_ask`
//! (rule id blastguard:unknown-verb-protected-path) asks on commands that
//! only *mention* a protected path (quoted free text, shell keywords,
//! read-only loops). Written by an agent that did not write the fix
//! (CLAUDE.md 2(a)).
//!
//! PROBES must be `Allow` and FAIL until the fix lands. CONTROLS are
//! destructive-capable unknown verbs on protected paths and must stay
//! non-`Allow` both before and after the fix.

use blastguard::detect;
use blastguard::model::Decision;
use serde_json::json;

fn bash(cmd: &str) -> Decision {
    detect::detect("Bash", Some(&json!({ "command": cmd })))
}

fn assert_allow(cmd: &str) {
    let d = bash(cmd);
    assert!(
        matches!(d, Decision::Allow),
        "false positive: must be Allow, got {d:?} for: {cmd}"
    );
}

fn assert_not_allow(cmd: &str) {
    let d = bash(cmd);
    assert!(
        !matches!(d, Decision::Allow),
        "control: must be Deny or Ask, got Allow for: {cmd}"
    );
}

// ------------------------------------------------------------ probes (RED)

#[test]
fn fp_backlog_add_title_mentions_protected_path() {
    assert_allow(r#"backlog add "harden .githooks/pre-commit" --notes x"#);
}

#[test]
fn fp_for_loop_only_reads_protected_path() {
    assert_allow(r#"for f in .githooks/pre-commit; do cat "$f"; done"#);
}

#[test]
fn fp_backlog_show_dot_git() {
    assert_allow("backlog show .git");
}

#[test]
fn fp_multiline_quoted_notes_mention_protected_path() {
    let cmd = "backlog add --title t --notes \"line1\n.githooks/pre-commit について\ngoal: x\"";
    assert_allow(cmd);
}

#[test]
fn fp_for_loop_over_plain_words_with_protected_path_in_list() {
    // protected path in the list, only read via echo
    assert_allow(r#"for f in a .githooks/pre-commit b; do echo "$f"; done"#);
}

// ---------------------------------------------------------------- controls

#[test]
fn control_patch_on_protected_path_not_allowed() {
    assert_not_allow("patch .githooks/pre-commit fix.diff");
}

#[test]
fn control_ed_on_protected_path_not_allowed() {
    assert_not_allow("ed .githooks/pre-commit");
}

#[test]
fn control_sponge_on_protected_path_not_allowed() {
    assert_not_allow("sponge .githooks/pre-commit");
}

#[test]
fn control_rsync_into_protected_path_not_allowed() {
    assert_not_allow("rsync x .githooks/pre-commit");
}

#[test]
fn control_gzip_on_protected_path_not_allowed() {
    assert_not_allow("gzip .githooks/pre-commit");
}
