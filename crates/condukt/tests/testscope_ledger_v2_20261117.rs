#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! R6 of spec testscope-confirmed-run (backlog 64fe4e19): ledger.v2.tsv holds
//! the new records append-only; confirmation also appends a legacy 3-column row
//! to ledger.tsv; Pre alone never writes a legacy row.
//!
//! Written by an independent test author, not the implementer.
//!
//! DOES NOT PROVE: the v2 column order / record keyword beyond the substrings
//! asserted (the spec fixes neither); old-binary behaviour.

mod testscope_confirm_common;
use testscope_confirm_common::*;

// Double space: the raw tool_input.command differs from the normalized form.
const RAW: &str = "cargo  test -p foo --test bar";

fn legacy_rows(env: &Env) -> Vec<String> {
    std::fs::read_to_string(env.ledger())
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
#[ignore = "RED testscope-confirmed-run R6: no ledger.v2.tsv / no legacy row at confirmation"]
fn confirmation_appends_v2_confirm_and_legacy_three_column_row() {
    let env = Env::new();
    let root = repo();
    assert!(pre(&env, "1", RAW, root.path()).is_allow());
    post(&env, "1", RAW, root.path());
    let v2 = std::fs::read_to_string(env.v2()).expect("ledger.v2.tsv must exist");
    assert!(v2.contains("confirm"), "no confirm record: {v2:?}");
    let legacy = legacy_rows(&env);
    assert!(
        legacy.iter().any(|l| {
            let c: Vec<&str> = l.split('\t').collect();
            c.len() == 3 && c[1].contains(':') && c[2].contains("cargo test -p foo --test bar")
        }),
        "no legacy 3-column row: {legacy:?}"
    );
}

#[test]
#[ignore = "RED testscope-confirmed-run R6: Pre alone must not write a legacy row (it does at HEAD)"]
fn pre_alone_writes_no_legacy_row() {
    let env = Env::new();
    let root = repo();
    assert!(pre(&env, "1", RAW, root.path()).is_allow());
    assert!(
        legacy_rows(&env).iter().all(|l| !l.contains("--test bar")),
        "legacy row written at Pre: {:?}",
        legacy_rows(&env)
    );
    assert!(env.v2().exists(), "pending row belongs in ledger.v2.tsv");
}

#[test]
#[ignore = "RED testscope-confirmed-run R6: v2 pending row must carry raw command and payload cwd as columns"]
fn v2_pending_row_carries_raw_command_and_cwd_columns() {
    let env = Env::new();
    let root = repo();
    assert!(pre(&env, "1", RAW, root.path()).is_allow());
    let v2 = std::fs::read_to_string(env.v2()).expect("ledger.v2.tsv must exist");
    let cwd = root.path().to_str().unwrap();
    let fields: Vec<&str> = v2.lines().flat_map(|l| l.split('\t')).collect();
    assert!(fields.contains(&RAW), "raw command column missing: {v2:?}");
    assert!(fields.contains(&cwd), "cwd column missing: {v2:?}");
}

#[test]
#[ignore = "RED testscope-confirmed-run R6: existing v2 rows stay a byte prefix across later operations"]
fn existing_rows_are_a_byte_prefix_after_later_operations() {
    let env = Env::new();
    let root = repo();
    assert!(pre(&env, "1", RAW, root.path()).is_allow());
    let snap = std::fs::read(env.v2()).expect("ledger.v2.tsv must exist");
    let legacy_snap = std::fs::read(env.ledger()).unwrap_or_default();
    post(&env, "1", RAW, root.path());
    let mid = std::fs::read(env.v2()).unwrap();
    pre(&env, "2", RAW, root.path());
    post(&env, "9", RAW, root.path());
    let end = std::fs::read(env.v2()).unwrap();
    assert!(mid.starts_with(&snap) && mid.len() > snap.len());
    assert!(end.starts_with(&mid));
    let legacy_end = std::fs::read(env.ledger()).unwrap_or_default();
    assert!(legacy_end.starts_with(&legacy_snap));
}
