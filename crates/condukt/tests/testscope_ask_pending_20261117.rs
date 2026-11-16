#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! R3 of spec testscope-confirmed-run (backlog 64fe4e19): a rerun whose only
//! prior is pending (never confirmed) resolves to Ask, not Deny, and the Ask
//! text never contains "deny".
//!
//! Written by an independent test author, not the implementer.
//!
//! DOES NOT PROVE: headless / subagent resolution of Ask (R10), ledger layout.

mod testscope_confirm_common;
use std::hash::{Hash, Hasher};
use testscope_confirm_common::*;

const CMD: &str = "cargo test -p foo --test bar";

#[test]
fn a_pre_only_then_pre_is_ask() {
    let env = Env::new();
    let root = repo();
    let first = pre(&env, "1", CMD, root.path());
    assert!(first.is_allow(), "first Pre: {}", first.stdout);
    let second = pre(&env, "2", CMD, root.path());
    assert!(second.is_ask(), "expected Ask, got: {}", second.stdout);
}

#[test]
fn ask_output_never_contains_deny() {
    let env = Env::new();
    let root = repo();
    pre(&env, "1", CMD, root.path());
    let second = pre(&env, "2", CMD, root.path());
    assert!(second.is_ask(), "expected Ask, got: {}", second.stdout);
    let texts = second.texts();
    assert!(!texts.is_empty(), "Ask must carry a reason");
    for t in texts {
        assert!(!t.to_lowercase().contains("deny"), "reason says deny: {t}");
    }
}

/// Key of the clean tree exactly as testscope's legacy ledger line wrote it.
fn legacy_line(root: &std::path::Path, cmd: &str) -> String {
    let top = git(root, &["rev-parse", "--show-toplevel"]);
    let head = git(root, &["rev-parse", "HEAD"]);
    let mut h = std::collections::hash_map::DefaultHasher::new();
    String::new().hash(&mut h); // git diff HEAD --binary
    String::new().hash(&mut h); // untracked list
    format!("{top}\t{head}:{:016x}\t{cmd}\n", h.finish())
}

#[test]
fn b_legacy_three_column_row_only_is_pending_so_ask() {
    let env = Env::new();
    let root = repo();
    std::fs::write(env.ledger(), legacy_line(root.path(), CMD)).unwrap();
    let out = pre(&env, "1", CMD, root.path());
    assert!(out.is_ask(), "expected Ask, got: {}", out.stdout);
}

#[test]
fn c_pre_without_tool_use_id_is_allowed_but_never_promoted() {
    let env = Env::new();
    let root = repo();
    let first = fire(&env, Ev::Pre, "s", None, CMD, root.path());
    assert!(first.is_allow(), "first Pre: {}", first.stdout);
    let second = fire(&env, Ev::Pre, "s", None, CMD, root.path());
    assert!(second.is_ask(), "expected Ask, got: {}", second.stdout);
}

#[test]
fn ask_pre_row_is_promoted_by_its_own_post() {
    let env = Env::new();
    let root = repo();
    assert!(pre(&env, "1", CMD, root.path()).is_allow());
    let asked = pre(&env, "2", CMD, root.path());
    assert!(asked.is_ask(), "expected Ask, got: {}", asked.stdout);
    post(&env, "2", CMD, root.path());
    let after = pre(&env, "3", CMD, root.path());
    assert!(after.is_deny(), "confirmed run must Deny: {}", after.stdout);
}

#[test]
fn ask_pre_without_post_stays_pending() {
    let env = Env::new();
    let root = repo();
    assert!(pre(&env, "1", CMD, root.path()).is_allow());
    assert!(pre(&env, "2", CMD, root.path()).is_ask());
    let third = pre(&env, "3", CMD, root.path());
    assert!(third.is_ask(), "expected Ask, got: {}", third.stdout);
}

/// Control (passes at HEAD): the very first Pre is allowed silently.
#[test]
fn control_first_pre_is_allowed() {
    let env = Env::new();
    let root = repo();
    let out = pre(&env, "1", CMD, root.path());
    assert_eq!(out.code, Some(0));
    assert!(out.is_allow(), "{}", out.stdout);
}
