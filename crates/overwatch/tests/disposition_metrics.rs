//! Integration test for review-effectiveness metrics
//! (`overwatch record-disposition` + `overwatch review-metrics`).
//!
//! Seeds findings and dispositions via the REAL CLI (`record-finding` /
//! `record-disposition`, mirroring `tests/review_queue.rs`) to prove the
//! subcommands are wired end-to-end, then overwrites the resulting JSONL
//! ledgers on disk with EXACT, controlled timestamps (the CLI stamps
//! wall-clock `ts`/`resolved_ts`, which this test cannot control precisely)
//! so the false-positive rate / agreement rate / median-latency assertions
//! below are matched against genuinely hand-computed expected values, not a
//! wall-clock-dependent range.
//!
//! Before `record-disposition` / `review-metrics` existed, this test fails to
//! even run (clap rejects the unrecognized subcommands with a non-zero exit,
//! tripping the `assert!(out.status.success())` in `run_ow`) — that is the
//! RED; wiring them in is the GREEN.
//!
//! Both the store path and the project key are sandboxed via a temp HOME +
//! temp cwd, so nothing real is touched and the test is hermetic.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

fn make_sandbox(tag: &str) -> (PathBuf, PathBuf) {
    let n = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    let base = std::env::temp_dir().join(format!(
        "overwatch-disp-test-{tag}-{}-{n}",
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

/// Recursively find a file named `name` under `dir` (the sandboxed HOME —
/// used to locate the JSONL ledger the CLI just wrote, without needing to
/// replicate `harness_core::projkey`'s hashing scheme in this test).
fn find_file(dir: &Path, name: &str) -> PathBuf {
    fn walk(dir: &Path, name: &str, found: &mut Option<PathBuf>) {
        if found.is_some() {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, name, found);
            } else if path.file_name().and_then(|f| f.to_str()) == Some(name) {
                *found = Some(path.clone());
            }
            if found.is_some() {
                return;
            }
        }
    }
    let mut found = None;
    walk(dir, name, &mut found);
    found.unwrap_or_else(|| panic!("could not find {name} under {}", dir.display()))
}

#[test]
fn review_metrics_computes_fp_rate_agreement_rate_and_median_latency() {
    let (home, work) = make_sandbox("core");

    // Seed 4 findings + 4 dispositions via the REAL CLI, proving the
    // subcommands parse and dispatch correctly (this is the RED->GREEN
    // proof: an unwired subcommand makes `run_ow` panic on a non-zero exit).
    for (id, summary) in [
        ("F-1", "finding one"),
        ("F-2", "finding two"),
        ("F-3", "finding three"),
        ("F-4", "finding four"),
    ] {
        run_ow(
            &home,
            &work,
            &[
                "record-finding",
                "--finding-id",
                id,
                "--source",
                "reviewgate",
                "--summary",
                summary,
            ],
        );
    }
    for (id, verdict) in [
        ("F-1", "confirmed"),
        ("F-2", "confirmed"),
        ("F-3", "dismissed"),
        ("F-4", "false-positive"),
    ] {
        run_ow(
            &home,
            &work,
            &[
                "record-disposition",
                "--finding-id",
                id,
                "--verdict",
                verdict,
                "--reviewer",
                "alice",
            ],
        );
    }

    // Overwrite the ledgers on disk with EXACT, controlled timestamps so the
    // metric assertions below are hand-computable (the CLI itself stamps
    // wall-clock ts, which this test does not control).
    let findings_path = find_file(&home, "review_findings.jsonl");
    let findings_jsonl = [
        (1000i64, "F-1"),
        (2000i64, "F-2"),
        (3000i64, "F-3"),
        (4000i64, "F-4"),
    ]
    .iter()
    .map(|(ts, id)| {
        serde_json::json!({
            "finding_id": id, "source": "reviewgate", "summary": "s", "ts": ts,
        })
        .to_string()
    })
    .collect::<Vec<_>>()
    .join("\n")
        + "\n";
    std::fs::write(&findings_path, findings_jsonl).unwrap();

    let dispositions_path = find_file(&home, "dispositions.jsonl");
    // Hand-computed latencies (resolved_ts - finding ts): 10, 50, 100, 200.
    let dispositions_jsonl = [
        (1010i64, "F-1", "confirmed"),
        (2050i64, "F-2", "confirmed"),
        (3100i64, "F-3", "dismissed"),
        (4200i64, "F-4", "false_positive"),
    ]
    .iter()
    .map(|(resolved_ts, id, verdict)| {
        serde_json::json!({
            "finding_id": id, "verdict": verdict, "reviewer": "alice", "resolved_ts": resolved_ts,
        })
        .to_string()
    })
    .collect::<Vec<_>>()
    .join("\n")
        + "\n";
    std::fs::write(&dispositions_path, dispositions_jsonl).unwrap();

    let metrics_out = run_ow(&home, &work, &["review-metrics", "--json"]);
    let m: Value = serde_json::from_str(&metrics_out).expect("review-metrics --json must parse");

    // Hand-computed: 1 false-positive of 4 = 0.25 ; 2 confirmed of 4 = 0.5.
    assert_eq!(m["total"], 4);
    assert_eq!(m["false_positive_rate"].as_f64().unwrap(), 0.25, "{m}");
    assert_eq!(m["agreement_rate"].as_f64().unwrap(), 0.5, "{m}");
    assert_eq!(m["by_verdict"]["confirmed"], 2);
    assert_eq!(m["by_verdict"]["dismissed"], 1);
    assert_eq!(m["by_verdict"]["false_positive"], 1);

    // Hand-computed median: latencies sorted [10, 50, 100, 200] (even count)
    // -> mean of the two middle values (50, 100) via integer division = 75.
    assert_eq!(m["median_latency_secs"].as_i64().unwrap(), 75, "{m}");
}

#[test]
fn review_metrics_empty_store_is_zeros_and_nulls_not_an_error() {
    let (home, work) = make_sandbox("empty");
    let out = run_ow(&home, &work, &["review-metrics", "--json"]);
    let m: Value = serde_json::from_str(&out).expect("review-metrics --json must parse even empty");

    assert_eq!(m["total"], 0);
    assert!(m["false_positive_rate"].is_null());
    assert!(m["agreement_rate"].is_null());
    assert!(m["median_latency_secs"].is_null());
    assert_eq!(m["by_verdict"]["confirmed"], 0);
    assert_eq!(m["by_verdict"]["dismissed"], 0);
    assert_eq!(m["by_verdict"]["false_positive"], 0);
}

#[test]
fn record_disposition_rejects_unknown_verdict() {
    let (home, work) = make_sandbox("bad-verdict");
    let out = Command::new(overwatch_bin())
        .args([
            "record-disposition",
            "--finding-id",
            "F-1",
            "--verdict",
            "maybe",
            "--reviewer",
            "alice",
        ])
        .env("HOME", &home)
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .current_dir(&work)
        .output()
        .expect("failed to spawn overwatch binary");
    assert!(
        !out.status.success(),
        "an unknown --verdict must be rejected with a non-zero exit"
    );
}

#[test]
fn median_latency_join_uses_earliest_ts_on_refound_finding() {
    // The Continuous-Audit loop re-records a still-open finding across
    // rounds with the SAME finding-id. The disposition's latency must be
    // measured from the EARLIEST (first-seen) ts, not the most recent
    // re-affirmation.
    let (home, work) = make_sandbox("earliest-ts");

    run_ow(
        &home,
        &work,
        &[
            "record-finding",
            "--finding-id",
            "F-refound",
            "--source",
            "continuous-audit",
            "--summary",
            "round-1 wording",
        ],
    );
    run_ow(
        &home,
        &work,
        &[
            "record-disposition",
            "--finding-id",
            "F-refound",
            "--verdict",
            "confirmed",
            "--reviewer",
            "alice",
        ],
    );

    // Overwrite the findings ledger with TWO records for the same id: a
    // later-recorded (larger ts) round-1 entry and an earlier (smaller ts)
    // round-2 entry, out of insertion order, to make sure the join picks the
    // MINIMUM ts regardless of record order.
    let findings_path = find_file(&home, "review_findings.jsonl");
    let findings_jsonl = format!(
        "{}\n{}\n",
        serde_json::json!({"finding_id": "F-refound", "source": "continuous-audit", "summary": "round-2", "ts": 500}),
        serde_json::json!({"finding_id": "F-refound", "source": "continuous-audit", "summary": "round-1", "ts": 100}),
    );
    std::fs::write(&findings_path, findings_jsonl).unwrap();

    let dispositions_path = find_file(&home, "dispositions.jsonl");
    let dispositions_jsonl = format!(
        "{}\n",
        serde_json::json!({"finding_id": "F-refound", "verdict": "confirmed", "reviewer": "alice", "resolved_ts": 150}),
    );
    std::fs::write(&dispositions_path, dispositions_jsonl).unwrap();

    let metrics_out = run_ow(&home, &work, &["review-metrics", "--json"]);
    let m: Value = serde_json::from_str(&metrics_out).unwrap();
    assert_eq!(m["total"], 1);
    // Hand-computed: earliest ts = 100, resolved_ts = 150 -> latency = 50.
    // A join that (incorrectly) used the latest ts (500) would yield -350.
    assert_eq!(m["median_latency_secs"].as_i64().unwrap(), 50, "{m}");
}
