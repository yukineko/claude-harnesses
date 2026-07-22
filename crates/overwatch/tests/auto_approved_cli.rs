// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used)]
//! CLI integration test for the condukt-gate-decisions bridge
//! (`overwatch auto-approved`, `review_gate_decisions.rs`).
//!
//! `harness_core::config::base_dir` resolves `~/.condukt` via
//! `dirs::home_dir()`, which on unix reads the `HOME` env var — the SAME
//! override mechanism `tests/review_escalation.rs` already uses to sandbox
//! condukt's foreign state. So this test seeds a temp-HOME condukt
//! `gate-decisions.jsonl` by hand at its FLAT default path (condukt's own
//! on-disk shape — no condukt binary/crate needed, keeping the dependency
//! direction intact), runs the REAL `overwatch auto-approved --json` CLI
//! against that sandboxed HOME, and asserts the auto-only count, the sample
//! size, and determinism across two runs with the same seed.

use serde_json::Value;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

fn make_sandbox(tag: &str) -> (PathBuf, PathBuf) {
    let n = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    let base = std::env::temp_dir().join(format!(
        "overwatch-auto-approved-test-{tag}-{}-{n}",
        std::process::id()
    ));
    let home = base.join("home");
    let work = base.join("work");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&work).unwrap();
    (home, work)
}

fn overwatch_bin() -> &'static str {
    env!("CARGO_BIN_EXE_overwatch")
}

#[test]
fn auto_approved_reports_only_auto_rows_and_is_deterministic() {
    let (home, work) = make_sandbox("basic");

    // Seed condukt's gate-decisions.jsonl directly at its DEFAULT (FLAT,
    // no project-key segment) path, mirroring condukt's own
    // `gatelog::decisions_path` / `config.rs::base_dir` derivation, without
    // depending on the condukt crate.
    let decisions_path = home
        .join(".condukt")
        .join("state")
        .join("gate-decisions.jsonl");
    std::fs::create_dir_all(decisions_path.parent().unwrap()).unwrap();

    let m = 8usize;
    let k = 3usize;
    let mut lines = Vec::new();
    for i in 0..m {
        lines.push(format!(
            r#"{{"question":"Q{i}","options":["a","b"],"recommend_index":0,"chosen":"a","policy":"auto","created_at":{}}}"#,
            100 + i
        ));
    }
    // One non-auto row that must be excluded.
    lines.push(
        r#"{"question":"Qesc","options":["x"],"recommend_index":0,"chosen":"x","policy":"escalate","created_at":999}"#
            .to_string(),
    );
    std::fs::write(&decisions_path, lines.join("\n")).unwrap();

    let run = |seed: &str| {
        let out = Command::new(overwatch_bin())
            .args([
                "auto-approved",
                "--json",
                "--sample",
                &k.to_string(),
                "--seed",
                seed,
            ])
            .env("HOME", &home)
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .current_dir(&work)
            .output()
            .expect("failed to spawn overwatch binary");
        assert!(
            out.status.success(),
            "auto-approved exited non-zero: stderr={}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).expect("stdout not utf8")
    };

    let stdout1 = run("42");
    let stdout2 = run("42");

    let v1: Value = serde_json::from_str(&stdout1).expect("auto-approved --json must be parseable");
    let v2: Value = serde_json::from_str(&stdout2).expect("auto-approved --json must be parseable");

    assert_eq!(v1["count"], m as i64, "only auto rows counted: {v1}");
    assert_eq!(v1["sample_size"], k as i64);
    assert_eq!(v1["seed"], 42);
    assert_eq!(v1["since"], Value::Null);

    let sample1 = v1["sample"].as_array().expect("sample must be an array");
    assert_eq!(sample1.len(), k);
    for row in sample1 {
        assert_eq!(
            row["policy"], "auto",
            "non-auto row leaked into sample: {row}"
        );
    }

    // Determinism: same seed => byte-identical sample across two runs.
    assert_eq!(
        v1["sample"], v2["sample"],
        "same (population, k, seed) must yield an identical sample"
    );
    assert_eq!(stdout1, stdout2, "full JSON output must be byte-identical");
}

#[test]
fn auto_approved_missing_journal_reports_zero() {
    let (home, work) = make_sandbox("missing");
    // Intentionally never seed gate-decisions.jsonl.

    let out = Command::new(overwatch_bin())
        .args(["auto-approved", "--json"])
        .env("HOME", &home)
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .current_dir(&work)
        .output()
        .expect("failed to spawn overwatch binary");
    assert!(
        out.status.success(),
        "auto-approved exited non-zero: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("stdout not utf8");
    let v: Value = serde_json::from_str(&stdout).expect("auto-approved --json must be parseable");
    assert_eq!(v["count"], 0);
    assert_eq!(v["sample_size"], 0);
    assert_eq!(v["sample"].as_array().unwrap().len(), 0);
}
