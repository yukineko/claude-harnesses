#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Independent controls for backlog b1e6f2a7: B.5 (`detect/ast/b5.rs`
//! `unwrap_at`) must model `env -S STRING` / `--split-string=STRING` /
//! `--split-string STRING` / `-SSTRING`: the literal is split per GNU env
//! rules and the resulting command judged normally; unresolvable operands stay
//! Undetermined. Written by an agent that did not write the fix (CLAUDE.md
//! 2(a)). One test per input so a failure names it.
//!
//! Expected verdicts come from the observed verdict of the equivalent plain
//! command (probed at base): `rm -rf ~`, `bash -c "rm -rf ~"`,
//! `env -i rm -rf ~` and `git reset --hard` are Deny.
//!
//! DOES NOT PROVE: GNU escape handling (`\_`, `\c`, quotes inside -S) — not
//! pinned because the intended splitter coverage is unspecified; subagent-mode
//! (Deny for Undetermined) verdicts.

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
        "must be Allow, got {d:?} for: {cmd}"
    );
}

fn assert_deny(cmd: &str) {
    let d = bash(cmd);
    assert!(d.is_deny(), "must be Deny, got {d:?} for: {cmd}");
}

fn assert_not_allow(cmd: &str) {
    let d = bash(cmd);
    assert!(
        !matches!(d, Decision::Allow),
        "must not be Allow, got {d:?} for: {cmd}"
    );
}

// ---- benign: resolves statically, must be Allow (RED before fix) ----

#[test]
fn allow_env_s_ls_la() {
    assert_allow("env -S 'ls -la'");
}

#[test]
fn allow_env_split_string_eq_git_status() {
    assert_allow("env --split-string='git status --short'");
}

#[test]
fn allow_env_split_string_separate_operand() {
    assert_allow("env --split-string 'ls -la'");
}

#[test]
fn allow_env_s_attached() {
    assert_allow("env -S'echo hi'");
}

// GNU env: the split words are inserted into argv and option/NAME=VALUE
// processing continues over them, so `FOO=1` is an assignment, then `ls`.
#[test]
fn allow_env_s_assignment_then_command() {
    assert_allow("env -S 'FOO=1 ls'");
}

// ---- harmful: severity must equal the plain command's (Deny) ----

#[test]
fn deny_env_s_rm_rf_home() {
    assert_deny("env -S 'rm -rf ~'");
}

#[test]
fn deny_env_s_bash_c_rm_rf_home() {
    assert_deny(r#"env -S 'bash -c "rm -rf ~"'"#);
}

#[test]
fn deny_env_s_git_reset_hard() {
    assert_deny("env -S 'git reset --hard'");
}

#[test]
fn deny_env_i_s_rm_rf_home() {
    assert_deny("env -i -S 'rm -rf ~'");
}

#[test]
fn deny_env_split_string_eq_rm_rf_home() {
    assert_deny("env --split-string='rm -rf ~'");
}

#[test]
fn deny_env_s_attached_rm_rf_home() {
    assert_deny("env -S'rm -rf ~'");
}

// ---- undetermined: must never be Allow ----

#[test]
fn not_allow_env_s_shell_expansion_operand() {
    assert_not_allow(r#"env -S "$CMD""#);
}

// GNU env -S expands ${VAR} from its own environment, so the argv is unknown
// even though plain `ls ${HOME}` is Allow.
#[test]
fn not_allow_env_s_dollar_brace_var() {
    assert_not_allow("env -S 'ls ${HOME}'");
}
