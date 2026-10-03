#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! backlog 71a880a2: neither filing (`add`) nor a work-free close (`done` /
//! `fail`) can record whether the claim was OBSERVED (command + output) or only
//! inferred/judged, so a suspicion is stored and closed exactly like a fact.
//!
//! This pins reading (a) of the ticket (a structural evidence-kind field is
//! required). Property asserted: each of `add`, `done`, `fail` exposes an
//! evidence option in its CLI. RED = none of their `--help` texts mention one.
//!
//! Written by an independent auditor, not an implementer.

mod common;

use std::process::{Command, Stdio};

fn help(sub: &str) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_backlog"))
        .env("PATH", common::path_with_condukt_shim())
        .args([sub, "--help"])
        .stdin(Stdio::null())
        .output()
        .expect("binary runs");
    assert!(
        out.status.success(),
        "precondition: `backlog {sub} --help` must succeed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_lowercase()
}

#[test]
#[ignore = "backlog 71a880a2: open defect, remove ignore when fixed"]
fn add_and_closes_can_record_observed_vs_inferred_evidence() {
    let missing: Vec<&str> = ["add", "done", "fail"]
        .into_iter()
        .filter(|sub| !help(sub).contains("evidence"))
        .collect();
    assert!(
        missing.is_empty(),
        "no evidence-kind (observed / inferred / judgment) option on: {missing:?}"
    );
}
