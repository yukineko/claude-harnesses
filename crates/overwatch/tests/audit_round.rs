// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Integration test for the Continuous-Audit round metrics ledger (2630b4c5).
//!
//! Seeds >=2 rounds through the REAL CLI (`overwatch audit-round record`), where
//! round 1 surfaces MORE new-findings than round 2, then runs
//! `overwatch audit-metrics --json` and asserts the per-round new-findings
//! trend is decreasing (round1 > round2), `converging` is true, and the
//! per-round + overall closure-rate (regression_tests_added / confirmed) is
//! computed correctly.
//!
//! Separately it records a CONFIRMED finding via `overwatch record-finding` and
//! asserts it surfaces in `overwatch review-queue --json` — proving the audit
//! loop's two deterministic write paths (round metrics + findings ingestion)
//! both land in overwatch's readable stores.
//!
//! Sandboxed via a temp HOME + temp cwd so nothing real is touched and the test
//! is hermetic (same pattern as `review_queue.rs`).

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

fn make_sandbox(tag: &str) -> (PathBuf, PathBuf) {
    let n = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    let base = std::env::temp_dir().join(format!(
        "overwatch-audit-test-{tag}-{}-{n}",
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

fn run_ow(home: &Path, work: &Path, args: &[&str]) -> String {
    let out = Command::new(overwatch_bin())
        .args(args)
        .env("HOME", home)
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
fn audit_metrics_reports_decreasing_trend_and_closure_rate() {
    let (home, work) = make_sandbox("metrics");

    // Round 1: MORE new-findings than round 2, half of confirmed converted.
    // new=6 confirmed=4 tests=2  => per-round closure = 2/4 = 0.5
    run_ow(
        &home,
        &work,
        &[
            "audit-round",
            "record",
            "--round",
            "1",
            "--target",
            "specguard,stuckguard",
            "--new-findings",
            "6",
            "--confirmed",
            "4",
            "--regression-tests-added",
            "2",
        ],
    );

    // Round 2: FEWER new-findings, all confirmed converted.
    // new=2 confirmed=2 tests=2  => per-round closure = 2/2 = 1.0
    run_ow(
        &home,
        &work,
        &[
            "audit-round",
            "record",
            "--round",
            "2",
            "--target",
            "specguard",
            "--new-findings",
            "2",
            "--confirmed",
            "2",
            "--regression-tests-added",
            "2",
        ],
    );

    let stdout = run_ow(&home, &work, &["audit-metrics", "--json"]);
    let report: Value =
        serde_json::from_str(&stdout).expect("audit-metrics --json must be parseable");

    let rounds = report["rounds"]
        .as_array()
        .expect("report has a rounds array");
    assert_eq!(rounds.len(), 2, "both rounds must be recorded");

    // Per-round new-findings trend is decreasing: round1 (6) > round2 (2).
    let n1 = rounds[0]["new_findings"].as_u64().unwrap();
    let n2 = rounds[1]["new_findings"].as_u64().unwrap();
    assert_eq!(n1, 6);
    assert_eq!(n2, 2);
    assert!(
        n1 > n2,
        "audit-metrics must show the decreasing new-findings trend: {n1} !> {n2}"
    );

    // Convergence flag is set (non-increasing new-findings across rounds).
    assert_eq!(
        report["converging"], true,
        "a decreasing new-findings trend must report converging=true"
    );

    // Per-round closure-rate = regression_tests_added / confirmed.
    assert_eq!(rounds[0]["closure_rate"].as_f64().unwrap(), 0.5);
    assert_eq!(rounds[1]["closure_rate"].as_f64().unwrap(), 1.0);

    // Overall closure-rate = (2+2)/(4+2) = 4/6.
    let overall = report["closure_rate"].as_f64().unwrap();
    assert!(
        (overall - 4.0 / 6.0).abs() < 1e-9,
        "overall closure-rate should be 4/6, got {overall}"
    );
    assert_eq!(report["cumulative_confirmed"].as_u64().unwrap(), 6);
    assert_eq!(
        report["cumulative_regression_tests_added"]
            .as_u64()
            .unwrap(),
        4
    );
    assert_eq!(report["total_new_findings"].as_u64().unwrap(), 8);
}

#[test]
fn same_finder_verifier_model_records_warning_finding_in_review_queue() {
    // finder==verifier is a MUST violation (shared blind spot). Recording a
    // round with the SAME finder/verifier model must deterministically surface a
    // high-severity warning finding in the review queue (fail-soft: the round is
    // still recorded, the loop is never broken).
    let (home, work) = make_sandbox("same-model");

    let out = run_ow(
        &home,
        &work,
        &[
            "audit-round",
            "record",
            "--round",
            "2026W30",
            "--target",
            "specguard",
            "--new-findings",
            "1",
            "--confirmed",
            "1",
            "--regression-tests-added",
            "1",
            "--finder-model",
            "claude-3-5-sonnet",
            "--verifier-model",
            "claude-3-5-sonnet",
        ],
    );
    // The round itself is still recorded (never-break-a-turn).
    let rec: Value = serde_json::from_str(out.lines().next().unwrap_or("{}"))
        .expect("audit-round record emits JSON");
    assert_eq!(rec["recorded"], true, "the round must still be recorded");

    // And a model-collision warning finding surfaces in the review queue.
    let stdout = run_ow(&home, &work, &["review-queue", "--json"]);
    let arr: Value = serde_json::from_str(&stdout).expect("review-queue --json parseable");
    let rows = arr.as_array().expect("review-queue is an array");
    let finding_row = rows
        .iter()
        .find(|r| {
            r["kind"] == "ai-finding" && r["identifier"] == "audit-round-model-collision-2026W30"
        })
        .expect("a model-collision warning finding must appear in the review queue");
    let summary = finding_row["summary"].as_str().unwrap();
    assert!(
        summary.contains("[high]"),
        "model collision is high severity (review-queue prefixes [high]): {finding_row:?}"
    );
    assert!(
        summary.contains("MUST"),
        "finding summary should name the MUST violation: {finding_row:?}"
    );
}

#[test]
fn distinct_finder_verifier_model_records_no_collision_finding() {
    // Distinct finder/verifier models satisfy the diversity MUST => NO warning
    // finding. Backward-compat: omitting both model args also records nothing.
    let (home, work) = make_sandbox("distinct-model");

    run_ow(
        &home,
        &work,
        &[
            "audit-round",
            "record",
            "--round",
            "2026W31",
            "--target",
            "specguard",
            "--finder-model",
            "claude-3-5-sonnet",
            "--verifier-model",
            "claude-3-5-opus",
        ],
    );
    // A second round with NO model args at all (backward compatible).
    run_ow(
        &home,
        &work,
        &[
            "audit-round",
            "record",
            "--round",
            "2026W32",
            "--target",
            "specguard",
        ],
    );

    let stdout = run_ow(&home, &work, &["review-queue", "--json"]);
    let arr: Value = serde_json::from_str(&stdout).expect("review-queue --json parseable");
    let rows = arr.as_array().expect("review-queue is an array");
    assert!(
        !rows.iter().any(|r| r["kind"] == "ai-finding"
            && r["identifier"]
                .as_str()
                .is_some_and(|id| id.starts_with("audit-round-model-collision-"))),
        "distinct/omitted models must NOT record a collision finding: {rows:?}"
    );
}

#[test]
fn confirmed_finding_recorded_by_audit_loop_surfaces_in_review_queue() {
    // The other deterministic write path of a round: recording a CONFIRMED
    // finding. It must land in the findings store and surface in review-queue.
    let (home, work) = make_sandbox("finding-to-queue");

    run_ow(
        &home,
        &work,
        &[
            "record-finding",
            "--finding-id",
            "CA-round1-001",
            "--source",
            "continuous-audit",
            "--severity",
            "high",
            "--summary",
            "confirmed: unchecked unwrap in specguard similarity path",
            "--file",
            "crates/specguard/src/similarity.rs",
        ],
    );

    let stdout = run_ow(&home, &work, &["review-queue", "--json"]);
    let arr: Value = serde_json::from_str(&stdout).expect("review-queue --json parseable");
    let rows = arr.as_array().expect("review-queue is an array");

    let finding_row = rows
        .iter()
        .find(|r| r["kind"] == "ai-finding" && r["identifier"] == "CA-round1-001")
        .expect("the confirmed audit finding must appear in the review queue");
    assert!(
        finding_row["summary"]
            .as_str()
            .unwrap()
            .contains("continuous-audit"),
        "finding summary should carry its source: {finding_row:?}"
    );
}
