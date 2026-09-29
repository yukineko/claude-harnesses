// このファイルは丸ごと integration test なので unwrap/expect を許可する。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! 7c27050b: a finding that already has a disposition must leave
//! `overwatch review-queue`.
//!
//! `reconcile.rs` documents that findings "only leave the `review-queue` when
//! a human runs `record-disposition`". At HEAD the queue reads the hot
//! `review_findings.jsonl` verbatim and never joins `dispositions.jsonl`; only
//! `compact-findings` (which no automation invokes) removes resolved rows. So a
//! closed finding keeps being listed as if it still needed review, and the
//! queue depth / closure picture is inflated by resolved items.
//!
//! Hermetic: temp HOME + temp cwd, real binary, real CLI round trip.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static N: AtomicU64 = AtomicU64::new(0);

fn sandbox(tag: &str) -> (PathBuf, PathBuf) {
    let n = N.fetch_add(1, Ordering::Relaxed);
    let base = std::env::temp_dir().join(format!("ow-rqdisp-{tag}-{}-{n}", std::process::id()));
    let home = base.join("home");
    let work = base.join("work");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&work).unwrap();
    (home, work)
}

fn ow(home: &Path, work: &Path, args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_overwatch"))
        .args(args)
        .env("HOME", home)
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .current_dir(work)
        .output()
        .expect("spawn overwatch");
    assert!(
        out.status.success(),
        "overwatch {args:?} exited non-zero: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

fn record_finding(home: &Path, work: &Path, id: &str) {
    ow(
        home,
        work,
        &[
            "record-finding",
            "--verdict",
            "confirmed",
            "--finding-id",
            id,
            "--source",
            "continuous-audit",
            "--severity",
            "high",
            // distinct summaries: the queue also collapses same-fingerprint rows
            "--summary",
            &format!("distinct summary for {id}"),
            "--file",
            &format!("src/{id}.rs"),
        ],
    );
}

fn queue_ids(home: &Path, work: &Path) -> Vec<String> {
    let v: Value = serde_json::from_str(&ow(home, work, &["review-queue", "--json"])).unwrap();
    v.as_array()
        .expect("array")
        .iter()
        .filter(|r| r["kind"] == "ai-finding")
        .map(|r| r["identifier"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn dispositioned_finding_leaves_the_review_queue_open_one_stays() {
    let (home, work) = sandbox("leaves");
    record_finding(&home, &work, "F-CLOSED");
    record_finding(&home, &work, "F-OPEN");

    // Precondition (control): both are queued before any disposition.
    let before = queue_ids(&home, &work);
    assert!(before.contains(&"F-CLOSED".into()), "{before:?}");
    assert!(before.contains(&"F-OPEN".into()), "{before:?}");

    ow(
        &home,
        &work,
        &[
            "record-disposition",
            "--finding-id",
            "F-CLOSED",
            "--verdict",
            "confirmed",
            "--reviewer",
            "test",
        ],
    );

    let after = queue_ids(&home, &work);
    assert!(
        !after.contains(&"F-CLOSED".into()),
        "a dispositioned finding must leave the review queue; still listed: {after:?}"
    );
    // Control: a fix that hides everything must not pass.
    assert!(
        after.contains(&"F-OPEN".into()),
        "an undispositioned finding must stay in the queue: {after:?}"
    );
}

#[test]
fn every_disposition_verdict_closes_the_finding() {
    let (home, work) = sandbox("verdicts");
    for (id, verdict) in [
        ("F-DIS", "dismissed"),
        ("F-FP", "false-positive"),
        ("F-CONF", "confirmed"),
    ] {
        record_finding(&home, &work, id);
        ow(
            &home,
            &work,
            &[
                "record-disposition",
                "--finding-id",
                id,
                "--verdict",
                verdict,
                "--reviewer",
                "test",
            ],
        );
    }
    record_finding(&home, &work, "F-STILL");
    let after = queue_ids(&home, &work);
    assert_eq!(
        after,
        vec!["F-STILL".to_string()],
        "only the undispositioned finding may remain"
    );
}
