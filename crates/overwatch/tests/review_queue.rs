//! Integration test for the unified review surface (`overwatch review-queue`).
//!
//! Seeds all three sources end-to-end through the REAL CLI + store round trip —
//! a systemic gate violation (recorded across distinct tasks so it escalates),
//! a canary rollback event, and an AI-review finding — then runs
//! `review-queue --json` and asserts every seeded source-type appears in the
//! merged output, time-ordered newest-first.
//!
//! Both the store path (`$HOME/.overwatch/<project-key>/overwatch/`) and the
//! project key (derived from the cwd) are sandboxed via a temp HOME + temp cwd,
//! so nothing real is touched and the test is hermetic.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

fn make_sandbox(tag: &str) -> (PathBuf, PathBuf) {
    let n = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    let base = std::env::temp_dir().join(format!(
        "overwatch-rq-test-{tag}-{}-{n}",
        std::process::id()
    ));
    let home = base.join("home");
    let work = base.join("work");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&work).unwrap();
    (home, work)
}

/// Path to the built overwatch binary (cargo sets CARGO_BIN_EXE_<name>).
fn overwatch_bin() -> &'static str {
    env!("CARGO_BIN_EXE_overwatch")
}

/// Run the overwatch binary with a sandboxed HOME + cwd, returning stdout.
fn run_ow(home: &Path, work: &Path, args: &[&str]) -> String {
    let out = Command::new(overwatch_bin())
        .args(args)
        .env("HOME", home)
        // Ensure session id is deterministic-ish / not read from the ambient env.
        .env("CLAUDE_CODE_SESSION_ID", "")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .current_dir(work)
        .output()
        .expect("failed to spawn overwatch binary");
    assert!(
        out.status.success(),
        "overwatch {:?} exited non-zero: stderr={}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("overwatch stdout not utf8")
}

#[test]
fn review_queue_merges_all_three_sources_time_ordered() {
    let (home, work) = make_sandbox("all-three");

    // --- Source 1: a SYSTEMIC violation ------------------------------------
    // Record the same blastguard signature across three DISTINCT tasks so the
    // item-B recurrence path escalates it to systemic (default threshold=3,
    // spanning >1 task). Recorded via the real CLI, not hand-crafted JSONL.
    for task in ["task-a", "task-b", "task-c"] {
        run_ow(
            &home,
            &work,
            &[
                "record-violation",
                "--source",
                "blastguard",
                "--discriminator",
                "rm-rf",
                "--task",
                task,
                "--session",
                task, // distinct session per task too
            ],
        );
    }

    // --- Source 2: a canary ROLLBACK event ---------------------------------
    run_ow(
        &home,
        &work,
        &[
            "record-rollback",
            "--plugin",
            "overwatch",
            "--from-version",
            "0.1.7",
            "--to-version",
            "0.1.8",
            "--stage",
            "1",
            "--reason",
            "systemic",
        ],
    );

    // --- Source 3: an AI-review FINDING ------------------------------------
    run_ow(
        &home,
        &work,
        &[
            "record-finding",
            "--finding-id",
            "F-001",
            "--source",
            "reviewgate",
            "--severity",
            "high",
            "--summary",
            "unchecked unwrap on user input",
            "--file",
            "src/foo.rs",
        ],
    );

    // --- Run review-queue --json -------------------------------------------
    let stdout = run_ow(&home, &work, &["review-queue", "--json"]);
    let arr: Value = serde_json::from_str(&stdout).expect("review-queue --json must be parseable");
    let rows = arr
        .as_array()
        .expect("review-queue --json must be an array");

    // Every seeded source-type must appear, discriminated by `kind`.
    let kinds: Vec<&str> = rows
        .iter()
        .map(|r| r["kind"].as_str().expect("each row has a string kind"))
        .collect();
    assert!(
        kinds.contains(&"systemic"),
        "systemic violation missing from queue: {kinds:?}"
    );
    assert!(
        kinds.contains(&"rollback"),
        "rollback event missing from queue: {kinds:?}"
    );
    assert!(
        kinds.contains(&"ai-finding"),
        "ai-finding missing from queue: {kinds:?}"
    );

    // Time-ordered newest-first: timestamps must be non-increasing.
    let ts: Vec<i64> = rows
        .iter()
        .map(|r| r["ts"].as_i64().expect("each row has an integer ts"))
        .collect();
    for w in ts.windows(2) {
        assert!(
            w[0] >= w[1],
            "review-queue must be ordered newest-first; got {ts:?}"
        );
    }

    // The key identifiers surface on their respective rows.
    let systemic_row = rows.iter().find(|r| r["kind"] == "systemic").unwrap();
    assert_eq!(systemic_row["identifier"], "blastguard:rm-rf");
    let rollback_row = rows.iter().find(|r| r["kind"] == "rollback").unwrap();
    assert_eq!(rollback_row["identifier"], "overwatch");
    let finding_row = rows.iter().find(|r| r["kind"] == "ai-finding").unwrap();
    assert_eq!(finding_row["identifier"], "F-001");
}

#[test]
fn review_queue_collapses_a_refound_finding_to_one_row() {
    // The Continuous-Audit loop re-records a still-confirmed finding every round
    // with the SAME finding-id. Through the real store round-trip, review-queue
    // must surface ONE row (the newest), not one row per round.
    let (home, work) = make_sandbox("dedup");

    // Round 1 and round 2 record the same id F-9; the round-2 summary is revised.
    run_ow(
        &home,
        &work,
        &[
            "record-finding",
            "--finding-id",
            "F-9",
            "--source",
            "continuous-audit",
            "--severity",
            "med",
            "--summary",
            "round-1 wording",
        ],
    );
    run_ow(
        &home,
        &work,
        &[
            "record-finding",
            "--finding-id",
            "F-9",
            "--source",
            "continuous-audit",
            "--severity",
            "high",
            "--summary",
            "round-2 revised wording",
        ],
    );
    // A DIFFERENT id must remain its own row.
    run_ow(
        &home,
        &work,
        &[
            "record-finding",
            "--finding-id",
            "F-10",
            "--source",
            "continuous-audit",
            "--summary",
            "a distinct finding",
        ],
    );

    let stdout = run_ow(&home, &work, &["review-queue", "--json"]);
    let arr: Value = serde_json::from_str(&stdout).expect("parseable");
    let rows = arr.as_array().unwrap();

    let ai: Vec<&Value> = rows.iter().filter(|r| r["kind"] == "ai-finding").collect();
    // F-9 (twice) collapses to one; F-10 stays → exactly two ai-finding rows.
    assert_eq!(
        ai.len(),
        2,
        "re-recorded finding must collapse; distinct id stays: {ai:?}"
    );
    let f9: Vec<&&Value> = ai.iter().filter(|r| r["identifier"] == "F-9").collect();
    assert_eq!(f9.len(), 1, "F-9 recorded twice must be ONE row");
    // The surfaced F-9 row reflects the newest (round-2) record.
    assert!(
        f9[0]["summary"]
            .as_str()
            .unwrap()
            .contains("round-2 revised wording"),
        "the newest record must win: {}",
        f9[0]["summary"]
    );
    assert!(
        ai.iter().any(|r| r["identifier"] == "F-10"),
        "distinct id F-10 must still appear"
    );
}

#[test]
fn review_queue_degrades_gracefully_when_a_source_is_empty() {
    // Only seed two of the three sources (NO ai-finding recorded — the arm with
    // no producer wired in production). The command must still succeed and
    // surface the two present sources rather than erroring on the missing one.
    let (home, work) = make_sandbox("degraded");

    for task in ["t1", "t2", "t3"] {
        run_ow(
            &home,
            &work,
            &[
                "record-violation",
                "--source",
                "propguard",
                "--discriminator",
                "PROP-009",
                "--task",
                task,
                "--session",
                task,
            ],
        );
    }
    run_ow(
        &home,
        &work,
        &[
            "record-rollback",
            "--plugin",
            "backlog",
            "--to-version",
            "0.2.0",
            "--reason",
            "raw",
        ],
    );

    let stdout = run_ow(&home, &work, &["review-queue", "--json"]);
    let arr: Value = serde_json::from_str(&stdout).expect("parseable even with an empty source");
    let rows = arr.as_array().unwrap();

    let kinds: Vec<&str> = rows.iter().map(|r| r["kind"].as_str().unwrap()).collect();
    assert!(
        kinds.contains(&"systemic"),
        "systemic must still appear: {kinds:?}"
    );
    assert!(
        kinds.contains(&"rollback"),
        "rollback must still appear: {kinds:?}"
    );
    // The absent AI-findings source contributes nothing — it must NOT appear,
    // and must NOT have errored the whole command (we got valid JSON above).
    assert!(
        !kinds.contains(&"ai-finding"),
        "no finding was recorded, so none should appear"
    );
}
