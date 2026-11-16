#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! R5 of spec testscope-confirmed-run (backlog 64fe4e19): confirmation matches
//! by tool_use_id; unmatched Posts and IO failure resolve restrictively.
//!
//! Written by an independent test author, not the implementer.
//!
//! UNTESTABLE here: a panic inside the confirm hook (no non-invasive
//! injection exists; the spec requires asking a human instead of judging).
//!
//! DOES NOT PROVE: ledger layout (only decisions are observed).

mod testscope_confirm_common;
use testscope_confirm_common::*;

const CMD: &str = "cargo test -p foo --test bar";

#[test]
#[ignore = "RED testscope-confirmed-run R5: Post(B) must promote only B; A stays pending (Ask)"]
fn post_promotes_only_its_own_tool_use_id() {
    let env = Env::new();
    let root = repo();
    // A runs on tree T1, B on tree T2 (same command).
    assert!(pre(&env, "A", CMD, root.path()).is_allow());
    edit_tree(root.path());
    assert!(pre(&env, "B", CMD, root.path()).is_allow());
    post(&env, "B", CMD, root.path());
    let t2 = pre(&env, "C", CMD, root.path());
    assert!(t2.is_deny(), "T2 confirmed by Post(B): {}", t2.stdout);
    restore_tree(root.path());
    let t1 = pre(&env, "D", CMD, root.path());
    assert!(t1.is_ask(), "T1 row A is still pending: {}", t1.stdout);
}

#[test]
#[ignore = "RED testscope-confirmed-run R5: unmatched Post records an unmatched row; rerun is Ask on any tree"]
fn unmatched_post_makes_later_reruns_ask_regardless_of_tree() {
    let env = Env::new();
    let root = repo();
    post(&env, "Z", CMD, root.path());
    let p = pre(&env, "P", CMD, root.path());
    assert!(p.is_ask(), "expected Ask, got: {}", p.stdout);
    edit_tree(root.path());
    let q = pre(&env, "Q", CMD, root.path());
    assert!(q.is_ask(), "unmatched is tree-independent: {}", q.stdout);
}

#[test]
#[ignore = "RED testscope-confirmed-run R5: one Post confirms every pending row of that tool_use_id"]
fn one_post_confirms_all_test_commands_of_one_call() {
    let env = Env::new();
    let root = repo();
    let a = "cargo test -p foo --test a";
    let b = "cargo test -p foo --test b";
    let both = format!("{a} && {b}");
    assert!(pre(&env, "1", &both, root.path()).is_allow());
    let early = pre(&env, "2", a, root.path());
    assert!(
        early.is_ask(),
        "unconfirmed row is pending: {}",
        early.stdout
    );
    post(&env, "1", &both, root.path());
    let ra = pre(&env, "3", a, root.path());
    assert!(ra.is_deny(), "a: {}", ra.stdout);
    let rb = pre(&env, "4", b, root.path());
    assert!(rb.is_deny(), "b: {}", rb.stdout);
}

#[test]
#[ignore = "RED testscope-confirmed-run R5: confirmed row on this tree beats an unmatched row; without it unmatched asks"]
fn confirmed_row_wins_over_unmatched_row() {
    let env = Env::new();
    let root = repo();
    assert!(pre(&env, "1", CMD, root.path()).is_allow());
    post(&env, "1", CMD, root.path());
    post(&env, "Z", CMD, root.path()); // unmatched
    let same = pre(&env, "2", CMD, root.path());
    assert!(same.is_deny(), "confirmed on this tree: {}", same.stdout);
    edit_tree(root.path());
    let other = pre(&env, "3", CMD, root.path());
    assert!(other.is_ask(), "no confirmed row here: {}", other.stdout);
}

#[test]
#[ignore = "RED testscope-confirmed-run R5: unwritable ledger on Post surfaces a systemMessage, never block; row stays pending"]
fn io_failure_on_post_is_surfaced_not_blocked_and_stays_pending() {
    use std::os::unix::fs::PermissionsExt;
    let env = Env::new();
    let root = repo();
    assert!(pre(&env, "1", CMD, root.path()).is_allow());
    assert!(env.v2().exists(), "Pre must have written ledger.v2.tsv");
    let ro = |p: &std::path::Path, m: u32| {
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(m)).unwrap()
    };
    ro(&env.v2(), 0o444);
    if env.ledger().exists() {
        ro(&env.ledger(), 0o444);
    }
    ro(env.dir.path(), 0o555);
    let p = post(&env, "1", CMD, root.path());
    let again = pre(&env, "2", CMD, root.path());
    ro(env.dir.path(), 0o755);
    assert_eq!(p.code, Some(0));
    let v = p.json().expect("Post must print a JSON systemMessage");
    assert!(v["systemMessage"].as_str().is_some_and(|s| !s.is_empty()));
    assert_ne!(v["decision"], "block", "Post must not block: {}", p.stdout);
    assert!(again.is_ask(), "row stayed pending: {}", again.stdout);
}
