// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Repro for backlog c33e3af4: `condukt diffrisk-report --json` with no journal
//! on disk (`journal_present: false`) reports `invocations` / `inspected` /
//! `blind_spots` as `null` (unknown) but `changed_symbols_total` /
//! `caller_sites_total` as `0` — "zero observed" for something never inspected.
//! The human-readable path already says "unknown" for the same state.

use std::path::PathBuf;
use std::process::Command;

struct Fx {
    base: PathBuf,
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn report_without_journal(tag: &str) -> (Fx, serde_json::Value) {
    let base =
        std::env::temp_dir().join(format!("condukt-bl-c33e3af4-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let fx = Fx { base: base.clone() };
    let repo = base.join("repo");
    let home = base.join("home");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let o = Command::new(env!("CARGO_BIN_EXE_condukt"))
        .args(["diffrisk-report", "--json"])
        .current_dir(&repo)
        .env("HOME", &home)
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .output()
        .expect("spawn condukt");
    assert!(o.status.success(), "diffrisk-report failed: {o:?}");
    let v: serde_json::Value = serde_json::from_slice(&o.stdout)
        .unwrap_or_else(|e| panic!("not JSON ({e}): {:?}", String::from_utf8_lossy(&o.stdout)));
    (fx, v)
}

/// Control: the fixture really is the no-journal case and the sibling counters
/// already say "unknown".
#[test]
fn control_no_journal_counts_are_null() {
    let (_fx, v) = report_without_journal("control");
    assert_eq!(v["journal_present"], serde_json::json!(false), "{v}");
    assert!(v["invocations"].is_null(), "{v}");
    assert!(v["inspected"].is_null(), "{v}");
    assert!(v["blind_spots"].is_null(), "{v}");
}

#[test]
#[ignore = "backlog c33e3af4: open defect, remove ignore when fixed"]
fn no_journal_totals_are_unknown_not_zero() {
    let (_fx, v) = report_without_journal("totals");
    assert_eq!(v["journal_present"], serde_json::json!(false), "{v}");
    assert!(
        v["changed_symbols_total"].is_null(),
        "nothing was inspected, so changed_symbols_total must be null (unknown), not a count: {v}"
    );
    assert!(
        v["caller_sites_total"].is_null(),
        "nothing was inspected, so caller_sites_total must be null (unknown), not a count: {v}"
    );
}
