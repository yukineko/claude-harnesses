#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! R9 of spec testscope-confirmed-run (backlog 64fe4e19): an unreadable or
//! unparseable ledger never reads as "no prior = allow".
//!
//! Written by an independent test author, not the implementer.
//!
//! DOES NOT PROVE: Ask vs Deny for a bad v2 file (the spec allows either; the
//! assertion is only "not Allow").

mod testscope_confirm_common;
use testscope_confirm_common::*;

const CMD: &str = "cargo test -p foo --test bar";

fn restrictive(o: &Out) -> bool {
    o.is_deny() || o.is_ask()
}

/// Control (passes at HEAD): the old ledger unreadable (a directory) is Deny.
#[test]
fn control_unreadable_legacy_ledger_is_deny() {
    let env = Env::new();
    let root = repo();
    std::fs::create_dir(env.ledger()).unwrap();
    let out = pre(&env, "1", CMD, root.path());
    assert!(out.is_deny(), "{}", out.stdout);
}

/// Control (passes at HEAD): no ledger at all allows the first run.
#[test]
fn control_no_ledger_allows_first_run() {
    let env = Env::new();
    let root = repo();
    assert!(pre(&env, "1", CMD, root.path()).is_allow());
}

#[test]
fn unreadable_v2_ledger_is_not_allow() {
    let env = Env::new();
    let root = repo();
    std::fs::create_dir(env.v2()).unwrap();
    let out = pre(&env, "1", CMD, root.path());
    assert!(restrictive(&out), "silent free run: {:?}", out.stdout);
}

#[test]
fn non_utf8_v2_ledger_is_not_allow() {
    let env = Env::new();
    let root = repo();
    std::fs::write(env.v2(), [0xff, 0xfe, b'\n']).unwrap();
    let out = pre(&env, "1", CMD, root.path());
    assert!(restrictive(&out), "silent free run: {:?}", out.stdout);
}

#[test]
fn unparseable_v2_row_is_not_allow() {
    let env = Env::new();
    let root = repo();
    std::fs::write(env.v2(), "zzz\tnot-a-record\n").unwrap();
    let out = pre(&env, "1", CMD, root.path());
    assert!(restrictive(&out), "silent free run: {:?}", out.stdout);
}
