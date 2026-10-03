// このファイルは丸ごと integration test なので unwrap/expect を許可する。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// Backlog 89544915 — observation-based auto-close of non-CA review findings.
// Independent tests (author != implementer). Black-box against the real
// `overwatch` binary with a temp HOME + temp cwd.
//
// CONTRACT ASSUMED (the implementer MUST match exactly):
//  * `overwatch record-disposition --finding-id ID --verdict resolved
//   --reviewer R [--evidence STR] [--observed-source STR]` — `resolved` is a
//   NEW accepted verdict. `--evidence` / `--observed-source` are optional.
//  * dispositions.jsonl rows serialize the verdict as `"resolved"` and carry
//    `evidence` (string) and `observed_source` (string) when given; rows
//    written before this change (neither key) must still deserialize.
//  * `overwatch review-metrics --json`:
//   - `resolved` rows are EXCLUDED from `agreement_rate`,
//   `false_positive_rate` and `by_verdict.{confirmed,dismissed,false_positive}`
//   (rates are `null` when there is no human disposition at all);
//   - a NEW top-level integer `auto_resolved` counts `resolved` rows;
//   - `closure_rate` / `closure_by_source` STILL count a resolved finding as
//   closed (it left the queue).
//  * a `resolved` disposition removes the finding from `review-queue --json`.
//  * `record-disposition` for an id that already has a row is an idempotent
//    no-op (exit 0, ledger unchanged): first writer wins, either order.
//  * `reconcile-fixed` never closes `gate-exec:` / `record-audit:` /
//    `specguard:` ids from commit messages (ruling R2).
//
// Status on CURRENT code is recorded in the report (RED vs GUARD per test).

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static N: AtomicU64 = AtomicU64::new(0);

fn sandbox(tag: &str) -> (PathBuf, PathBuf) {
    let n = N.fetch_add(1, Ordering::Relaxed);
    let base = std::env::temp_dir().join(format!("ow-89544915-{tag}-{}-{n}", std::process::id()));
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

fn ow(home: &Path, work: &Path, args: &[&str]) -> String {
    let out = ow_raw(home, work, args);
    assert!(
        out.status.success(),
        "overwatch {args:?} exited non-zero: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

fn record_finding(home: &Path, work: &Path, id: &str, source: &str) {
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
            source,
            "--severity",
            "high",
            "--summary",
            &format!("distinct summary for {id}"),
            "--file",
            &format!("src/{}.rs", id.replace(':', "_")),
        ],
    );
}

fn queue_ids(home: &Path, work: &Path) -> Vec<String> {
    let v: Value = serde_json::from_str(&ow(home, work, &["review-queue", "--json"])).unwrap();
    v.as_array()
        .unwrap()
        .iter()
        .filter(|r| r["kind"] == "ai-finding")
        .map(|r| r["identifier"].as_str().unwrap().to_string())
        .collect()
}

fn find_file(root: &Path, name: &str) -> Option<PathBuf> {
    for e in std::fs::read_dir(root).ok()?.flatten() {
        let p = e.path();
        if p.is_dir() {
            if let Some(f) = find_file(&p, name) {
                return Some(f);
            }
        } else if p.file_name().is_some_and(|n| n == name) {
            return Some(p);
        }
    }
    None
}

fn disposition_rows(home: &Path) -> Vec<Value> {
    match find_file(&home.join(".overwatch"), "dispositions.jsonl") {
        Some(p) => std::fs::read_to_string(p)
            .unwrap()
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect(),
        None => Vec::new(),
    }
}

fn disp(home: &Path, work: &Path, id: &str, verdict: &str) -> Output {
    ow_raw(
        home,
        work,
        &[
            "record-disposition",
            "--finding-id",
            id,
            "--verdict",
            verdict,
            "--reviewer",
            "t89544915",
        ],
    )
}

fn metrics(home: &Path, work: &Path) -> Value {
    serde_json::from_str(&ow(home, work, &["review-metrics", "--json"])).unwrap()
}

// ---------------------------------------------------------------- evidence

/// RED: `resolved` verdict + `--evidence` / `--observed-source` are accepted and
/// persisted on the row; the finding leaves the queue.
#[test]
fn resolved_disposition_persists_evidence_and_observed_source_and_closes() {
    let (home, work) = sandbox("evidence");
    let id = "gate-exec:run-1:t1";
    record_finding(&home, &work, id, "condukt-gate");
    record_finding(&home, &work, "gate-exec:run-1:t2", "condukt-gate");
    assert!(queue_ids(&home, &work).contains(&id.to_string()));

    let out = ow_raw(
        &home,
        &work,
        &[
            "record-disposition",
            "--finding-id",
            id,
            "--verdict",
            "resolved",
            "--reviewer",
            "condukt-reconcile",
            "--evidence",
            "{\"run\":\"run-1\",\"task\":\"t1\",\"status\":\"done\"}",
            "--observed-source",
            "condukt run-state",
        ],
    );
    assert!(
        out.status.success(),
        "record-disposition --verdict resolved --evidence ... must be accepted: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rows = disposition_rows(&home);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["finding_id"], id);
    assert_eq!(rows[0]["verdict"], "resolved");
    assert_eq!(rows[0]["observed_source"], "condukt run-state");
    let ev = rows[0]["evidence"].as_str().expect("evidence is a string");
    assert!(
        ev.contains("run-1") && ev.contains("t1") && ev.contains("done"),
        "{ev}"
    );
    // closed, and the sibling control stays queued
    let q = queue_ids(&home, &work);
    assert!(!q.contains(&id.to_string()), "{q:?}");
    assert!(q.contains(&"gate-exec:run-1:t2".to_string()), "{q:?}");
}

/// GUARD: a row written before the evidence fields existed still deserializes
/// (review-metrics reads it, exit 0, counted as a confirmed human verdict).
#[test]
fn legacy_disposition_row_without_evidence_fields_still_reads() {
    let (home, work) = sandbox("legacy");
    record_finding(&home, &work, "F-LEGACY", "reviewgate");
    // materialise the ledger dir via a real disposition on another id, then
    // append a hand-written legacy row.
    record_finding(&home, &work, "F-OTHER", "reviewgate");
    ow(
        &home,
        &work,
        &[
            "record-disposition",
            "--finding-id",
            "F-OTHER",
            "--verdict",
            "dismissed",
            "--reviewer",
            "t",
        ],
    );
    let p = find_file(&home.join(".overwatch"), "dispositions.jsonl").unwrap();
    let mut txt = std::fs::read_to_string(&p).unwrap();
    txt.push_str(
        "{\"finding_id\":\"F-LEGACY\",\"verdict\":\"confirmed\",\"reviewer\":\"old\",\"resolved_ts\":1}\n",
    );
    std::fs::write(&p, txt).unwrap();
    let m = metrics(&home, &work);
    assert_eq!(m["total"], 2, "{m}");
    assert_eq!(m["by_verdict"]["confirmed"], 1, "{m}");
}

// ----------------------------------------------------------------- T6

/// T6 (order A, RED): a human closed it first -> the auto path's `resolved`
/// attempt is accepted as an idempotent no-op; ONE row remains and it is the
/// human's.
#[test]
fn t6_manual_first_then_auto_resolved_leaves_single_human_row() {
    let (home, work) = sandbox("t6a");
    let id = "gate-exec:run-1:t1";
    record_finding(&home, &work, id, "condukt-gate");
    let a = disp(&home, &work, id, "confirmed");
    assert!(a.status.success());
    let b = disp(&home, &work, id, "resolved");
    assert!(
        b.status.success(),
        "`resolved` must be an accepted verdict, and a duplicate is an idempotent no-op: {}",
        String::from_utf8_lossy(&b.stderr)
    );
    let rows = disposition_rows(&home);
    assert_eq!(rows.len(), 1, "no duplicate row: {rows:?}");
    assert_eq!(
        rows[0]["verdict"], "confirmed",
        "first writer wins: {rows:?}"
    );
}

/// T6 (order B, RED): auto resolved first -> a later manual disposition is a
/// no-op; ONE row remains, the resolved one.
#[test]
fn t6_auto_resolved_first_then_manual_leaves_single_resolved_row() {
    let (home, work) = sandbox("t6b");
    let id = "record-audit:freshness:1000";
    record_finding(&home, &work, id, "record-audit");
    let a = disp(&home, &work, id, "resolved");
    assert!(
        a.status.success(),
        "`resolved` must be accepted: {}",
        String::from_utf8_lossy(&a.stderr)
    );
    let b = disp(&home, &work, id, "confirmed");
    assert!(b.status.success());
    let rows = disposition_rows(&home);
    assert_eq!(rows.len(), 1, "no duplicate row: {rows:?}");
    assert_eq!(
        rows[0]["verdict"], "resolved",
        "first writer wins: {rows:?}"
    );
}

// ----------------------------------------------------------------- T9

/// T9 (RED): a lone `resolved` is NOT a human verdict. No human disposition
/// exists, so the human rates are `null` (not 0.0, not 1.0), no human bucket
/// counts it, it is shown separately, and the finding still counts as closed.
#[test]
fn t9_resolved_only_is_not_a_human_verdict() {
    let (home, work) = sandbox("t9a");
    record_finding(&home, &work, "gate-exec:r:t1", "condukt-gate");
    let r = disp(&home, &work, "gate-exec:r:t1", "resolved");
    assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
    let m = metrics(&home, &work);
    assert!(m["agreement_rate"].is_null(), "agreement_rate: {m}");
    assert!(m["false_positive_rate"].is_null(), "fp rate: {m}");
    for k in ["confirmed", "dismissed", "false_positive"] {
        assert_eq!(
            m["by_verdict"][k], 0,
            "by_verdict.{k} counted resolved: {m}"
        );
    }
    assert_eq!(m["auto_resolved"], 1, "shown separately: {m}");
    assert_eq!(m["closure_rate"], 1.0, "a resolved finding is closed: {m}");
}

/// T9 (RED): mixed ledger. confirmed + false-positive + resolved: the human
/// denominator is 2 (resolved excluded) -> agreement 0.5, fp rate 0.5.
#[test]
fn t9_resolved_excluded_from_human_rate_denominator() {
    let (home, work) = sandbox("t9b");
    for (id, v) in [
        ("F-C", "confirmed"),
        ("F-FP", "false-positive"),
        ("gate-exec:r:t9", "resolved"),
    ] {
        record_finding(&home, &work, id, "x");
        let r = disp(&home, &work, id, v);
        assert!(
            r.status.success(),
            "{v}: {}",
            String::from_utf8_lossy(&r.stderr)
        );
    }
    let m = metrics(&home, &work);
    assert_eq!(m["agreement_rate"], 0.5, "{m}");
    assert_eq!(m["false_positive_rate"], 0.5, "{m}");
    assert_eq!(m["by_verdict"]["confirmed"], 1, "{m}");
    assert_eq!(m["by_verdict"]["false_positive"], 1, "{m}");
    assert_eq!(m["auto_resolved"], 1, "{m}");
    assert_eq!(m["closure_rate"], 1.0, "{m}");
}

// ----------------------------------------------------------------- T8

fn git(work: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
        .current_dir(work)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

/// T8 (GUARD on current code; must keep passing): a fix commit that NAMES a
/// gate-exec / record-audit / specguard id closes nothing (ruling R2: never
/// close these kinds by commit-message self-report). Control: the same commit
/// naming a CA- id DOES close it (the CA regex path is unchanged).
#[test]
fn t8_commit_naming_non_ca_ids_does_not_close_them() {
    let (home, work) = sandbox("t8");
    let ids = [
        "gate-exec:run-20260913-1:t4",
        "record-audit:freshness:1000",
        "specguard:untested:foo",
    ];
    for id in ids {
        record_finding(&home, &work, id, "x");
    }
    record_finding(&home, &work, "CA-overwatch-901", "continuous-audit");

    git(&work, &["init", "-q"]);
    git(&work, &["config", "commit.gpgsign", "false"]);
    std::fs::write(work.join("f.txt"), "1").unwrap();
    git(&work, &["add", "."]);
    git(
        &work,
        &[
            "commit",
            "-q",
            "-m",
            &format!("fix: resolve {} and CA-overwatch-901", ids.join(" and ")),
        ],
    );

    let out = ow(
        &home,
        &work,
        &["reconcile-fixed", "--last-n", "10", "--json"],
    );
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(
        v["reconciled"],
        serde_json::json!(["CA-overwatch-901"]),
        "only the CA id may be reconciled by commit message: {v}"
    );
    let q = queue_ids(&home, &work);
    for id in ids {
        assert!(q.contains(&id.to_string()), "{id} must stay open: {q:?}");
    }
    assert!(!q.contains(&"CA-overwatch-901".to_string()), "{q:?}");
}
