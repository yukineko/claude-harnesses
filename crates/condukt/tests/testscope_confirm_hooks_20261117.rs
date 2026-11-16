#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! R4 of spec testscope-confirmed-run (backlog 64fe4e19): PostToolUse and
//! PostToolUseFailure of a Bash test command confirm that it executed.
//!
//! Written by an independent test author, not the implementer.
//!
//! DOES NOT PROVE: the ledger format, which subcommand the Post hooks call
//! (read from hooks.json), or the Ask path (testscope_ask_pending_20261117.rs).

mod testscope_confirm_common;
use testscope_confirm_common::*;

const CMD: &str = "cargo test -p foo --test bar";

fn event_has_bash_entry(event: &str) -> bool {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("hooks/hooks.json");
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap();
    v["hooks"][event].as_array().into_iter().flatten().any(|e| {
        e["matcher"]
            .as_str()
            .unwrap_or("")
            .split('|')
            .any(|m| m == "Bash")
    })
}

#[test]
fn hooks_json_has_bash_posttooluse_entry() {
    assert!(event_has_bash_entry("PostToolUse"));
}

#[test]
fn hooks_json_has_bash_posttooluse_failure_entry() {
    assert!(event_has_bash_entry("PostToolUseFailure"));
}

#[test]
fn pre_then_post_confirms_and_rerun_is_denied() {
    let env = Env::new();
    let root = repo();
    let first = pre(&env, "1", CMD, root.path());
    assert!(first.is_allow(), "first Pre: {}", first.stdout);
    let p = post(&env, "1", CMD, root.path());
    assert_eq!(p.code, Some(0));
    assert!(
        p.decision().is_none() && !p.stdout.contains("\"block\""),
        "a Post must not emit a permission decision or block: {}",
        p.stdout
    );
    let again = pre(&env, "2", CMD, root.path());
    assert!(
        again.is_deny(),
        "rerun after confirmed run: {}",
        again.stdout
    );
}

#[test]
fn pre_then_post_failure_confirms_and_rerun_is_denied() {
    let env = Env::new();
    let root = repo();
    let first = pre(&env, "2", CMD, root.path());
    assert!(first.is_allow(), "first Pre: {}", first.stdout);
    let p = post_fail(&env, "2", CMD, root.path());
    assert_eq!(p.code, Some(0));
    assert!(
        p.decision().is_none() && !p.stdout.contains("\"block\""),
        "a PostToolUseFailure must not emit a permission decision or block: {}",
        p.stdout
    );
    let again = pre(&env, "3", CMD, root.path());
    assert!(
        again.is_deny(),
        "rerun after interrupted run: {}",
        again.stdout
    );
}

/// Control (passes at HEAD): a Bash call with no test command leaves no trace.
#[test]
fn control_non_test_post_and_failure_are_silent_and_write_nothing() {
    let env = Env::new();
    let root = repo();
    for out in [
        post(&env, "9", "echo hi", root.path()),
        post_fail(&env, "10", "echo hi", root.path()),
    ] {
        assert_eq!(out.code, Some(0));
        assert!(out.stdout.trim().is_empty(), "stdout: {}", out.stdout);
    }
    assert!(!env.ledger().exists(), "ledger.tsv written");
    assert!(!env.v2().exists(), "ledger.v2.tsv written");
}

/// Control (passes at HEAD): a Post without tool_use_id is not blocked.
#[test]
fn control_post_without_tool_use_id_does_not_block() {
    let env = Env::new();
    let root = repo();
    let out = fire(&env, Ev::Post, "s", None, CMD, root.path());
    assert_eq!(out.code, Some(0));
    assert!(!out.stdout.contains("\"block\""), "blocked: {}", out.stdout);
}
