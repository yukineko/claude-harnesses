// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `detect_bash` classified every `;`/`&&`/`||`/`|`/`&`-separated segment of a
//! Bash command INDEPENDENTLY, carrying no working-directory state between
//! them. Every protected pattern in `exclude.rs` is PATH-shaped, so a command
//! that first `cd`s (or `pushd`s, or aliases via a symlink) INTO a protected
//! directory and then operates on a BARE BASENAME evaded every protected-path
//! check and returned `Allow` — measured live on the shipped binary before
//! this file existed (ea1355f5).
//!
//! Every attack form below is paired with the DIRECT-PATH equivalent it
//! should now land on (the pre-existing Deny), plus the anti-vacuity
//! controls: a two-stage command into a NON-protected directory, and a
//! two-stage command that only READS a protected file, both of which must
//! stay `Allow` — otherwise "everything is Deny now" would be as vacuous a
//! false-clear as "everything is Allow" was before.

use blastguard::detect::detect;
use blastguard::model::Decision;
use serde_json::json;

fn bash(cmd: &str) -> Decision {
    detect("Bash", Some(&json!({ "command": cmd })))
}

/// A blocking verdict is Deny OR Ask: both stop the call from running
/// unreviewed, and `Decision::hardened` collapses Ask to Deny wherever no
/// human is present (see `model::Decision::hardened`).
fn assert_blocking(d: Decision, what: &str) {
    assert!(
        d.is_blocking(),
        "expected a blocking verdict for `{what}`, got {d:?}"
    );
}

// ---------------------------------------------------------------------------
// Controls. If these ever fail, no other assertion in this file means
// anything (round2_bypass_closure.rs's own preamble makes the same point).
// ---------------------------------------------------------------------------

#[test]
fn controls_direct_path_forms_are_already_denied() {
    // The direct-path twin of every cwd-prefix attack below. These already
    // passed before this fix; if they ever regress, the cwd-prefix fix below
    // cannot be trusted to have "landed on the same Deny".
    assert_blocking(bash("rm .githooks/pre-commit"), "rm .githooks/pre-commit");
    assert_blocking(
        bash("chmod -x .githooks/pre-commit"),
        "chmod -x .githooks/pre-commit",
    );
    assert_blocking(
        bash("chmod -r .githooks/pre-commit"),
        "chmod -r .githooks/pre-commit",
    );
    assert_blocking(bash("rm .claude/settings.json"), "rm .claude/settings.json");
}

#[test]
fn controls_two_stage_into_non_protected_dir_stays_allowed() {
    for cmd in [
        "cd src && cargo build",
        "cd src && cargo test",
        "cd src && ls",
        "pushd src && cargo build && popd",
    ] {
        assert_eq!(bash(cmd), Decision::Allow, "expected Allow for `{cmd}`");
    }
}

#[test]
fn controls_two_stage_read_of_a_protected_path_stays_allowed() {
    for cmd in [
        "cd .githooks && cat pre-commit",
        "cd .githooks && ls",
        "cd .claude && cat settings.json",
        "cd .githooks && wc -l pre-commit",
    ] {
        assert_eq!(bash(cmd), Decision::Allow, "expected Allow for `{cmd}`");
    }
}

// ---------------------------------------------------------------------------
// Attack forms. Each MUST be a blocking verdict (Deny or Ask, hardened to
// Deny with no human present) — measured `Allow` on the pre-fix binary.
// ---------------------------------------------------------------------------

#[test]
fn cd_into_protected_dir_then_rm_bare_basename_is_blocked() {
    assert_blocking(
        bash("cd .githooks && rm pre-commit"),
        "cd .githooks && rm pre-commit",
    );
}

#[test]
fn cd_into_protected_dir_then_chmod_removes_exec_bare_basename_is_blocked() {
    assert_blocking(
        bash("cd .githooks && chmod -x pre-commit"),
        "cd .githooks && chmod -x pre-commit",
    );
}

#[test]
fn cd_into_protected_dir_then_chmod_removes_read_bare_basename_is_blocked() {
    assert_blocking(
        bash("cd .githooks && chmod -r pre-commit"),
        "cd .githooks && chmod -r pre-commit",
    );
}

#[test]
fn cd_into_claude_dir_then_rm_settings_json_bare_basename_is_blocked() {
    assert_blocking(
        bash("cd .claude && rm settings.json"),
        "cd .claude && rm settings.json",
    );
}

#[test]
fn parenthesised_subshell_form_is_blocked() {
    // `(cd .githooks)` isolates the paren-stripping path: the WHOLE `cd`
    // invocation sits inside a subshell (own leading `(` glued to `cd`, own
    // trailing `)` glued to the target), and the `rm` runs in a later,
    // unparenthesised segment. This is deliberately NOT
    // `(cd .githooks && rm pre-commit)` — that spelling is (coincidentally)
    // already a blocking Ask on the pre-fix binary for an UNRELATED reason
    // (the mangled `(cd` head is itself treated as an unrecognised command
    // whose own operand, the bare literal `.githooks`, matches the
    // protected-path scan in `unknown_verb_protected_ask` — see
    // `parenthesised_subshell_wrapping_both_stages_is_blocked` below), which
    // would make it a vacuous RED test: it does not exercise cwd-prefix
    // propagation across the paren boundary at all. Splitting the target
    // (`.githooks)`) onto the closing paren defeats that literal match (the
    // trailing `)` breaks it) and isolates the real question — does the cwd
    // this analysis tracked INSIDE the subshell survive to the next,
    // un-parenthesised segment?
    assert_blocking(
        bash("(cd .githooks) && rm pre-commit"),
        "(cd .githooks) && rm pre-commit",
    );
}

#[test]
fn parenthesised_subshell_wrapping_both_stages_is_blocked() {
    // The more natural spelling of the same attack (both stages inside one
    // subshell). Kept as a regression/defense-in-depth check, NOT as
    // evidence of the cwd-prefix fix in isolation — see the note on
    // `parenthesised_subshell_form_is_blocked` above: this exact string was
    // already a blocking `Ask` before the fix, via the coincidence that its
    // mangled `(cd` head's own operand is the bare protected directory name.
    assert_blocking(
        bash("(cd .githooks && rm pre-commit)"),
        "(cd .githooks && rm pre-commit)",
    );
}

#[test]
fn pushd_popd_form_is_blocked() {
    assert_blocking(
        bash("pushd .githooks && rm pre-commit && popd"),
        "pushd .githooks && rm pre-commit && popd",
    );
}

#[test]
fn bash_dash_c_wrapper_form_is_blocked() {
    assert_blocking(
        bash(r#"bash -c "cd .githooks && rm pre-commit""#),
        r#"bash -c "cd .githooks && rm pre-commit""#,
    );
}

#[test]
fn symlink_alias_form_is_blocked() {
    assert_blocking(
        bash("ln -s .githooks hookslink && cd hookslink && rm pre-commit"),
        "ln -s .githooks hookslink && cd hookslink && rm pre-commit",
    );
}
