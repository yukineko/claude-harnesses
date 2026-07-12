//! CLI integration test for the condukt-escalation bridge
//! (`EntryKind::Escalation`, `review_escalation.rs`).
//!
//! `harness_core::config::base_dir` resolves `~/.condukt` via
//! `dirs::home_dir()`, which on unix reads the `HOME` env var — the SAME
//! override mechanism `tests/review_queue.rs` already uses to sandbox the
//! overwatch store itself. So this test seeds a temp-HOME condukt
//! `escalations.json` by hand (condukt's own on-disk shape — no condukt
//! binary/crate needed, keeping the dependency direction intact), runs the
//! REAL `overwatch review-queue --json` CLI against that sandboxed HOME, and
//! asserts exactly one `"kind":"escalation"` row surfaces (the open one; the
//! resolved one must not).

use serde_json::Value;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

fn make_sandbox(tag: &str) -> (PathBuf, PathBuf) {
    let n = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    let base = std::env::temp_dir().join(format!(
        "overwatch-rq-esc-test-{tag}-{}-{n}",
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
fn review_queue_surfaces_exactly_one_open_condukt_escalation() {
    let (home, work) = make_sandbox("open-plus-resolved");

    // Seed condukt's escalations.json directly at its DEFAULT path, derived
    // with the exact same harness_core symbols the production bridge uses
    // (harness_core::config::base_dir("condukt")/state/<project-key>/
    // escalations.json) — mirroring condukt's own `escalate.rs` derivation,
    // without depending on the condukt crate.
    std::env::set_var("HOME", &home);
    let repo_root = harness_core::projkey::repo_root(&work);
    let project_key = harness_core::projkey::project_key(&repo_root);
    std::env::remove_var("HOME");
    let escalations_path = home
        .join(".condukt")
        .join("state")
        .join(&project_key)
        .join("escalations.json");
    std::fs::create_dir_all(escalations_path.parent().unwrap()).unwrap();
    let body = r#"{
        "escalations": [
            {
                "id": "esc-open-1",
                "run": "runA",
                "task": "t1",
                "question": "Which migration strategy?",
                "options": ["a", "b"],
                "recommended": 0,
                "created_at": 500,
                "resolved": false
            },
            {
                "id": "esc-resolved-1",
                "run": "runA",
                "task": "t2",
                "question": "Already answered?",
                "options": ["x", "y"],
                "recommended": 0,
                "created_at": 400,
                "resolved": true,
                "chosen": "x"
            }
        ]
    }"#;
    std::fs::write(&escalations_path, body).unwrap();

    let out = Command::new(overwatch_bin())
        .args(["review-queue", "--json"])
        .env("HOME", &home)
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .current_dir(&work)
        .output()
        .expect("failed to spawn overwatch binary");
    assert!(
        out.status.success(),
        "review-queue exited non-zero: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("stdout not utf8");
    let arr: Value = serde_json::from_str(&stdout).expect("review-queue --json must be parseable");
    let rows = arr.as_array().expect("must be a JSON array");

    let escalation_rows: Vec<&Value> = rows.iter().filter(|r| r["kind"] == "escalation").collect();
    assert_eq!(
        escalation_rows.len(),
        1,
        "exactly the OPEN escalation must surface, the resolved one must not: {rows:?}"
    );
    assert_eq!(escalation_rows[0]["identifier"], "esc-open-1");
    assert_eq!(escalation_rows[0]["severity"], "high");
    assert_eq!(escalation_rows[0]["ts"], 500);
    assert!(escalation_rows[0]["summary"]
        .as_str()
        .unwrap()
        .contains("Which migration strategy?"));
}
