#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Repro: `overwatch audit-round record` with BOTH `--finder-model` and
//! `--verifier-model` omitted records the round with no trace that the
//! finder!=verifier diversity MUST was never checked. The round, the metrics
//! (`audit-metrics --json`) and the review queue are indistinguishable from a
//! round whose models were verified distinct. "Could not check diversity" is
//! undetermined; it must not be counted as "diversity satisfied".
//!
//! The existing test
//! `audit_round.rs::distinct_finder_verifier_model_records_no_collision_finding`
//! PINS the second half of its body (the round with no model args records
//! nothing). Only that fail-open half is contradicted here; its distinct-model
//! half (no collision finding for sonnet vs opus) stays correct and is not
//! touched. When the defect is fixed the omitted-models half of that pinning
//! test must be flipped in the same change.
//!
//! Correct behaviour asserted: an omitted-model round surfaces the unchecked
//! diversity SOMEWHERE observable -- non-zero exit, or the word "diversity" in
//! the record output, or a model-related finding in the review queue.
use serde_json::Value;
use std::process::Command;

#[test]
fn omitted_models_must_not_be_indistinguishable_from_verified_distinct() {
    let t = std::env::temp_dir().join(format!("ow-repro-audit-models-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&t);
    let home = t.join("home");
    let work = t.join("work");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&work).unwrap();

    let ow = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_overwatch"))
            .args(args)
            .env("HOME", &home)
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .current_dir(&work)
            .output()
            .unwrap()
    };

    let rec = ow(&[
        "audit-round",
        "record",
        "--round",
        "2026W32",
        "--target",
        "specguard",
    ]);
    let rec_text = format!(
        "{}{}",
        String::from_utf8_lossy(&rec.stdout),
        String::from_utf8_lossy(&rec.stderr)
    )
    .to_lowercase();

    let q = ow(&["review-queue", "--json"]);
    let rows: Value = serde_json::from_slice(&q.stdout).expect("review-queue --json parseable");
    let queue_text = rows.to_string().to_lowercase();

    let surfaced =
        !rec.status.success() || rec_text.contains("diversity") || queue_text.contains("model");
    assert!(
        surfaced,
        "a round recorded with NO finder/verifier model left no observable trace \
         that model diversity was unchecked (exit 0, no 'diversity' in output, \
         no model finding in the queue).\nrecord output={rec_text}\nqueue={queue_text}"
    );
}
