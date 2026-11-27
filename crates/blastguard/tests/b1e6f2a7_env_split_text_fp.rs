#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! False positive found 2026-11-27 while working backlog b1e6f2a7: the
//! `env -S` split-string handling in `detect.rs` ran for ANY whitespace token
//! spelled `env`, including the word `env` inside a double-quoted argument of
//! another command (`--title "b1e6f2a7 env -S"`). That text is never executed.
//! `env -S` handling must apply only where `env` is the command word as
//! parsed (`command_candidates`), not by matching text.
//!
//! The first three commands are the denied ones, verbatim (a flow driver
//! session denied the first two; the coordinator's main session denied the
//! third). The controls pin that a real `env -S` in command position (also
//! behind a wrapper, after `;` and `|`) is still judged.

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

#[test]
fn allow_claim_task_title_mentions_env_s() {
    assert_allow(
        r#"condukt state claim-task --run "flow-$X" --session "$X" --stateless --title "b1e6f2a7 env -S" --hashkey 86593bd23e82429c | head -3"#,
    );
}

#[test]
fn allow_git_commit_message_mentions_env_s() {
    assert_allow(
        "git commit -q -m \"blastguard: add b1e6f2a7 env -S B.5 controls (tests only)\n\nCo-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>\"",
    );
}

#[test]
fn allow_askrelay_reason_mentions_env_s_and_gt() {
    assert_allow(r#"askrelay dismiss X --reason "a env -S b > c""#);
}

#[test]
fn allow_echo_text_mentions_env_s() {
    assert_allow(r#"echo "use env -S 'ls -la' here""#);
}

// ---- controls: real `env -S` in command position is still judged ----

#[test]
fn deny_env_s_rm_plain() {
    assert_deny("env -S 'rm -rf ~'");
}

#[test]
fn deny_env_s_rm_after_semicolon() {
    assert_deny("echo hi; env -S 'rm -rf ~'");
}

#[test]
fn deny_env_s_rm_after_pipe() {
    assert_deny("echo hi | env -S 'rm -rf ~'");
}

#[test]
fn deny_env_s_rm_behind_wrapper() {
    assert_deny("nohup env -S 'rm -rf ~'");
}

#[test]
fn deny_env_s_rm_behind_assignment() {
    assert_deny("A=1 env -S 'rm -rf ~'");
}
