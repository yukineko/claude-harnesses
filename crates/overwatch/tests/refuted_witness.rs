// 丸ごと integration test なので unwrap/expect を許可する。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Backlog 80a46e9f. User ruling 2026-10-01: a REFUTED verdict stands only with
//! BOTH a machine probe result (claimed-unreachable branch replaced with a
//! panic, full suite run, result recorded as data) AND a human sign-off.
//! Anything less lands as `unverified`. A `reproduced` probe against a REFUTED
//! claim lands as `confirmed`. Interface: `record-finding --verdict refuted
//! --probe <result-file> --signed-off-by <name>`.
//!
//! ASSUMPTION (not in the ruling): the probe result file is JSON
//! `{"result":"not_reproduced"}` or `{"result":"reproduced"}`. The implementer
//! may change the spelling, but then must update `probe()` below only.
//!
//! All state lives in a temp HOME + temp project; the real ledger is never read.

use std::path::Path;
use std::process::{Command, Output};
use tempfile::TempDir;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_overwatch")
}

fn probe(dir: &Path, name: &str, body: &str) -> String {
    let p = dir.join(name);
    std::fs::write(&p, body).unwrap();
    p.to_string_lossy().into_owned()
}

fn record(home: &TempDir, project: &TempDir, id: &str, verdict: &str, extra: &[&str]) -> Output {
    Command::new(bin())
        .args([
            "record-finding",
            "--finding-id",
            id,
            "--source",
            "continuous-audit",
            "--summary",
            "claimed unreachable branch",
            "--verdict",
            verdict,
        ])
        .args(extra)
        .current_dir(project.path())
        .env("HOME", home.path())
        .output()
        .expect("spawn overwatch record-finding")
}

/// The verdict the CLI reports as stored. Fails the test if the call errored:
/// every case here must be accepted and resolved, never rejected.
fn stored_verdict(out: &Output) -> String {
    assert!(
        out.status.success(),
        "record-finding must exit 0 and resolve the verdict. stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    for v in ["confirmed", "refuted", "unverified"] {
        if stdout.contains(&format!("\"verdict\":\"{v}\"")) {
            return v.to_string();
        }
    }
    panic!("no verdict in stdout: {stdout}");
}

fn queue(home: &TempDir, project: &TempDir) -> String {
    let out = Command::new(bin())
        .arg("review-queue")
        .current_dir(project.path())
        .env("HOME", home.path())
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

const NOT_REPRODUCED: &str = r#"{"result":"not_reproduced"}"#;
const REPRODUCED: &str = r#"{"result":"reproduced"}"#;

#[test]
fn refuted_without_a_probe_is_stored_unverified() {
    let (h, p) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let out = record(&h, &p, "W-1", "refuted", &["--signed-off-by", "yuki"]);
    assert_eq!(stored_verdict(&out), "unverified");
    assert!(queue(&h, &p).contains("[UNVERIFIED]"));
}

#[test]
fn refuted_with_no_witness_flags_at_all_is_stored_unverified() {
    let (h, p) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let out = record(&h, &p, "W-0", "refuted", &[]);
    assert_eq!(stored_verdict(&out), "unverified");
}

#[test]
fn refuted_with_a_probe_but_no_signoff_is_stored_unverified() {
    let (h, p) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let f = probe(p.path(), "probe.json", NOT_REPRODUCED);
    let out = record(&h, &p, "W-2", "refuted", &["--probe", &f]);
    assert_eq!(stored_verdict(&out), "unverified");
}

#[test]
fn refuted_with_not_reproduced_probe_and_signoff_stands_control() {
    let (h, p) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let f = probe(p.path(), "probe.json", NOT_REPRODUCED);
    let out = record(
        &h,
        &p,
        "W-3",
        "refuted",
        &["--probe", &f, "--signed-off-by", "yuki"],
    );
    assert_eq!(stored_verdict(&out), "refuted");
}

#[test]
fn reproduced_probe_against_a_refuted_claim_lands_confirmed() {
    let (h, p) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let f = probe(p.path(), "probe.json", REPRODUCED);
    let out = record(
        &h,
        &p,
        "W-4",
        "refuted",
        &["--probe", &f, "--signed-off-by", "yuki"],
    );
    assert_eq!(stored_verdict(&out), "confirmed");
}

#[test]
fn witnessless_refuted_does_not_shadow_an_earlier_confirmed_in_the_queue() {
    let (h, p) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    assert_eq!(
        stored_verdict(&record(&h, &p, "W-5", "confirmed", &[])),
        "confirmed"
    );
    // Same id + summary => same row. The later witness-less REFUTED must not
    // displace the Confirmed, and must itself not be recorded as refuted.
    assert_eq!(
        stored_verdict(&record(&h, &p, "W-5", "refuted", &[])),
        "unverified"
    );
    let q = queue(&h, &p);
    assert!(q.contains("W-5"), "finding must stay visible: {q}");
    assert!(
        !q.contains("[UNVERIFIED]"),
        "earlier Confirmed must remain the row's verdict: {q}"
    );
}

#[test]
fn unreadable_or_unparseable_probe_is_never_refuted() {
    let (h, p) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let garbage = probe(p.path(), "garbage.json", "this is not json");
    let empty = probe(p.path(), "empty.json", "");
    let missing = p.path().join("nope.json").to_string_lossy().into_owned();
    let unknown = probe(p.path(), "unknown.json", r#"{"result":"probably_fine"}"#);
    for (i, f) in [garbage, empty, missing, unknown].iter().enumerate() {
        let out = record(
            &h,
            &p,
            &format!("W-6-{i}"),
            "refuted",
            &["--probe", f, "--signed-off-by", "yuki"],
        );
        assert_eq!(
            stored_verdict(&out),
            "unverified",
            "probe case {i} ({f}) must resolve restrictively"
        );
    }
}

#[test]
fn confirmed_and_unverified_are_unaffected_by_the_witness_rule_control() {
    let (h, p) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    assert_eq!(
        stored_verdict(&record(&h, &p, "W-7a", "confirmed", &[])),
        "confirmed"
    );
    assert_eq!(
        stored_verdict(&record(&h, &p, "W-7b", "unverified", &[])),
        "unverified"
    );
}
