// このファイルは丸ごと integration test なので unwrap/expect を許可する。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! 7c27050b, IMPLEMENTER-WRITTEN (not the independent RED author's test):
//! the review-queue's disposition join when `dispositions.jsonl` cannot be
//! read in full.
//!
//! The join must not HIDE anything it cannot justify, and must not pass the
//! list off as filtered: every finding stays listed, an in-band
//! `undetermined-source` row names `dispositions.jsonl`, stderr warns, and the
//! command exits 3 — on both the human and the `--json` channel.
//!
//! Hermetic: temp HOME + temp cwd, real binary, real CLI round trip.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static N: AtomicU64 = AtomicU64::new(0);

fn sandbox(tag: &str) -> (PathBuf, PathBuf) {
    let n = N.fetch_add(1, Ordering::Relaxed);
    let base =
        std::env::temp_dir().join(format!("ow-rqdisp-undet-{tag}-{}-{n}", std::process::id()));
    let home = base.join("home");
    let work = base.join("work");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&work).unwrap();
    (home, work)
}

fn ow_raw(home: &Path, work: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_overwatch"))
        .args(args)
        .env("HOME", home)
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .current_dir(work)
        .output()
        .expect("spawn overwatch")
}

fn ow_ok(home: &Path, work: &Path, args: &[&str]) {
    let out = ow_raw(home, work, args);
    assert!(
        out.status.success(),
        "overwatch {args:?} exited non-zero: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn record_finding(home: &Path, work: &Path, id: &str) {
    ow_ok(
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
            "--summary",
            &format!("distinct summary for {id}"),
            "--file",
            &format!("src/{id}.rs"),
        ],
    );
}

fn find_file(dir: &Path, name: &str) -> Option<PathBuf> {
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let p = e.path();
        if p.is_dir() {
            if let Some(f) = find_file(&p, name) {
                return Some(f);
            }
        } else if p.file_name().and_then(|n| n.to_str()) == Some(name) {
            return Some(p);
        }
    }
    None
}

/// Two findings, one dispositioned, then a garbage line appended to the
/// disposition ledger so it can no longer be read in full.
fn corrupted_disposition_sandbox(tag: &str) -> (PathBuf, PathBuf) {
    let (home, work) = sandbox(tag);
    record_finding(&home, &work, "F-CLOSED");
    record_finding(&home, &work, "F-OPEN");
    ow_ok(
        &home,
        &work,
        &[
            "record-disposition",
            "--finding-id",
            "F-CLOSED",
            "--verdict",
            "dismissed",
            "--reviewer",
            "test",
        ],
    );
    let ledger = find_file(&home, "dispositions.jsonl").expect("dispositions.jsonl was written");
    let mut txt = std::fs::read_to_string(&ledger).unwrap();
    txt.push_str("{this is not a disposition\n");
    std::fs::write(&ledger, txt).unwrap();
    (home, work)
}

#[test]
fn implementer_unreadable_dispositions_shows_unfiltered_and_says_so_in_json() {
    let (home, work) = corrupted_disposition_sandbox("json");
    let out = ow_raw(&home, &work, &["review-queue", "--json"]);
    assert_eq!(
        out.status.code(),
        Some(3),
        "an unfiltered queue must not exit 0: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rows: Value = serde_json::from_slice(&out.stdout).unwrap();
    let rows = rows.as_array().expect("still a bare array");

    let findings: Vec<&str> = rows
        .iter()
        .filter(|r| r["kind"] == "ai-finding")
        .map(|r| r["identifier"].as_str().unwrap())
        .collect();
    // Nothing is hidden on a guess: both findings stay listed, including the
    // one whose disposition is on the (now unreadable) ledger.
    assert!(findings.contains(&"F-CLOSED"), "{findings:?}");
    assert!(findings.contains(&"F-OPEN"), "{findings:?}");

    let marker = rows
        .iter()
        .find(|r| r["kind"] == "undetermined-source" && r["identifier"] == "dispositions.jsonl")
        .unwrap_or_else(|| panic!("no undetermined-source row for dispositions.jsonl: {rows:?}"));
    let summary = marker["summary"].as_str().unwrap();
    assert!(summary.contains("NOT FILTERED"), "{summary}");

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("WARNING") && stderr.contains("dispositions.jsonl"),
        "{stderr}"
    );
}

#[test]
fn implementer_unreadable_dispositions_is_visible_in_human_output() {
    let (home, work) = corrupted_disposition_sandbox("human");
    let out = ow_raw(&home, &work, &["review-queue"]);
    assert_eq!(out.status.code(), Some(3));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("[undetermined-source]") && stdout.contains("dispositions.jsonl"),
        "{stdout}"
    );
    assert!(stdout.contains("<F-CLOSED>"), "{stdout}");
    assert!(stdout.contains("<F-OPEN>"), "{stdout}");
}

#[test]
fn implementer_readable_dispositions_emit_no_marker_and_exit_zero() {
    // Control for the two tests above: the marker is caused by the corrupt
    // line, not by the join itself.
    let (home, work) = sandbox("control");
    record_finding(&home, &work, "F-CLOSED");
    record_finding(&home, &work, "F-OPEN");
    ow_ok(
        &home,
        &work,
        &[
            "record-disposition",
            "--finding-id",
            "F-CLOSED",
            "--verdict",
            "dismissed",
            "--reviewer",
            "test",
        ],
    );
    let out = ow_raw(&home, &work, &["review-queue", "--json"]);
    assert_eq!(out.status.code(), Some(0));
    let rows: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(
        !rows
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["kind"] == "undetermined-source"),
        "{rows:?}"
    );
}
