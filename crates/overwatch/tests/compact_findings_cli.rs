//! CLI integration test for `overwatch compact-findings` (non-lossy
//! compaction/rotation of the append-only `review_findings.jsonl` hot store
//! into a cold `review_findings_archive.jsonl`).
//!
//! Mirrors the HOME-sandbox + real-binary + `find_file` idiom of
//! `tests/disposition_metrics.rs` / `tests/auto_approved_cli.rs`: seeds the
//! hot findings store with 3 OPEN findings + 2 findings whose ids are then
//! resolved (one bridged, one dispositioned with a resolved_ts), runs the
//! real `overwatch compact-findings --json` binary, and asserts the hot
//! file is bounded to OPEN items while the archive holds the resolved ones
//! and the review-metrics latency join still sees the dispositioned finding
//! (join over hot plus archive). A second run must be a byte-identical
//! no-op.
//!
//! Before this task the `compact-findings` subcommand does not exist: clap
//! rejects it with a non-zero exit, tripping the `assert!(out.status.success())`
//! in `run_ow` below (RED). Wiring it in is the GREEN.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

fn make_sandbox(tag: &str) -> (PathBuf, PathBuf) {
    let n = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    let base = std::env::temp_dir().join(format!(
        "overwatch-compact-findings-test-{tag}-{}-{n}",
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

/// Run the overwatch binary with a sandboxed HOME + cwd, returning stdout.
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

/// Try to find a file named `name` under `dir`; `None` if absent (used to
/// assert a file was NOT (re)created, e.g. when nothing was compacted).
fn try_find_file(dir: &Path, name: &str) -> Option<PathBuf> {
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
    found
}

#[test]
fn compact_findings_archives_resolved_and_keeps_hot_bounded_to_open() {
    let (home, work) = make_sandbox("core");

    // Seed 5 findings via the real CLI: F-1..F-3 stay OPEN, F-4 will be
    // bridged, F-5 will be dispositioned.
    for id in ["F-1", "F-2", "F-3", "F-4", "F-5"] {
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
                "s",
            ],
        );
    }

    // Overwrite the hot findings ledger with EXACT, controlled timestamps so
    // the review-metrics latency assertion below is hand-computable.
    let findings_path = find_file(&home, "review_findings.jsonl");
    let findings_jsonl = [
        (1000i64, "F-1"),
        (2000i64, "F-2"),
        (3000i64, "F-3"),
        (4000i64, "F-4"),
        (5000i64, "F-5"),
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
    std::fs::write(&findings_path, &findings_jsonl).unwrap();

    // F-4 resolved via bridging: seed bridged_findings.jsonl directly
    // (mirrors `store::BridgedFinding { finding_id, ts }`).
    let bridged_path = findings_path
        .parent()
        .unwrap()
        .join("bridged_findings.jsonl");
    std::fs::write(
        &bridged_path,
        format!("{}\n", serde_json::json!({"finding_id": "F-4", "ts": 4500})),
    )
    .unwrap();

    // F-5 resolved via disposition (with a resolved_ts, for the latency
    // join): seed dispositions.jsonl directly.
    let dispositions_path = findings_path.parent().unwrap().join("dispositions.jsonl");
    std::fs::write(
        &dispositions_path,
        format!(
            "{}\n",
            serde_json::json!({
                "finding_id": "F-5", "verdict": "confirmed",
                "reviewer": "alice", "resolved_ts": 5050,
            })
        ),
    )
    .unwrap();

    // Run compact-findings and assert the report.
    let out1 = run_ow(&home, &work, &["compact-findings", "--json"]);
    let v1: Value = serde_json::from_str(&out1).expect("compact-findings --json must parse");
    assert_eq!(v1["open"], 3, "{v1}");
    assert_eq!(v1["archived"], 2, "{v1}");
    assert_eq!(v1["already_archived"], 0, "{v1}");

    // Hot file now holds ONLY the 3 open findings.
    let hot_contents = std::fs::read_to_string(&findings_path).unwrap();
    let hot_ids: Vec<String> = hot_contents
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| {
            let v: Value = serde_json::from_str(l).unwrap();
            v["finding_id"].as_str().unwrap().to_string()
        })
        .collect();
    assert_eq!(hot_ids, vec!["F-1", "F-2", "F-3"], "hot={hot_contents}");

    // Archive holds the 2 resolved findings, hot order preserved.
    let archive_path = findings_path
        .parent()
        .unwrap()
        .join("review_findings_archive.jsonl");
    let archive_contents = std::fs::read_to_string(&archive_path).unwrap();
    let archive_ids: Vec<String> = archive_contents
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| {
            let v: Value = serde_json::from_str(l).unwrap();
            v["finding_id"].as_str().unwrap().to_string()
        })
        .collect();
    assert_eq!(
        archive_ids,
        vec!["F-4", "F-5"],
        "archive={archive_contents}"
    );

    // review-metrics latency join must still count the dispositioned finding
    // (join now reads hot plus archive): F-5 ts=5000, resolved_ts=5050 ->
    // latency 50.
    let metrics_out = run_ow(&home, &work, &["review-metrics", "--json"]);
    let m: Value = serde_json::from_str(&metrics_out).expect("review-metrics --json must parse");
    assert_eq!(m["total"], 1, "{m}");
    assert_eq!(
        m["median_latency_secs"].as_i64().unwrap(),
        50,
        "latency join must still see the archived F-5 finding: {m}"
    );

    // Run compact-findings a SECOND time: must be a no-op (archived == 0)
    // and both files byte-identical to the first run's output.
    let hot_before_second = std::fs::read_to_string(&findings_path).unwrap();
    let archive_before_second = std::fs::read_to_string(&archive_path).unwrap();

    let out2 = run_ow(&home, &work, &["compact-findings", "--json"]);
    let v2: Value = serde_json::from_str(&out2).expect("compact-findings --json must parse");
    assert_eq!(v2["open"], 3, "{v2}");
    assert_eq!(v2["archived"], 0, "second run must archive nothing: {v2}");
    assert_eq!(v2["already_archived"], 2, "{v2}");

    let hot_after_second = std::fs::read_to_string(&findings_path).unwrap();
    let archive_after_second = std::fs::read_to_string(&archive_path).unwrap();
    assert_eq!(
        hot_before_second, hot_after_second,
        "hot file must be byte-identical after a no-op second run"
    );
    assert_eq!(
        archive_before_second, archive_after_second,
        "archive file must be byte-identical after a no-op second run"
    );
}

#[test]
fn compact_findings_missing_hot_store_is_a_noop_zero_report() {
    let (home, work) = make_sandbox("missing");
    // Intentionally never seed any findings.

    let out = run_ow(&home, &work, &["compact-findings", "--json"]);
    let v: Value = serde_json::from_str(&out).expect("compact-findings --json must parse");
    assert_eq!(v["open"], 0);
    assert_eq!(v["archived"], 0);
    assert_eq!(v["already_archived"], 0);

    // A missing hot file must not create an archive file out of thin air.
    assert!(
        try_find_file(&home, "review_findings_archive.jsonl").is_none(),
        "no-op must not create an archive file"
    );
}
