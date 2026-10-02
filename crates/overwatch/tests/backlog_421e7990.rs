#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! backlog 421e7990: `review-metrics`'s stale-undisposed `Undetermined` arm
//! (disposition_cli.rs, `undetermined.push("stale-undisposed join")`) was
//! documented in-code as "NOT COVERED BY ANY TEST ... UNREACHABLE from this
//! call site short of a TOCTOU race ... the case a test here cannot stage
//! deterministically".
//!
//! It CAN be staged: make `dispositions.jsonl` a named pipe whose writer
//! serves a valid (empty) ledger to the FIRST open and an undecodable line to
//! the SECOND. `review-metrics` reads the disposition ledger exactly twice —
//! once for its own `Known` gate, once again inside
//! `reconcile::stale_undisposed_count` — so the second read is the mid-run
//! change the comment says cannot be staged. The arm must then print a `null`
//! stale count, name the join in `undetermined_sources`, and exit 3; a zero
//! here would be the silence the arm exists to prevent.
//!
//! RED observed by deleting the arm's `undetermined.push(..)` (exit 0, empty
//! `undetermined_sources`); GREEN at HEAD.

#![cfg(unix)]

use std::io::Write;
use std::process::Command;
use std::time::Duration;

#[test]
fn stale_undisposed_join_undetermined_arm_is_reachable_and_not_silent() {
    let home = tempfile::tempdir().unwrap();
    let proj = tempfile::tempdir().unwrap();
    let bin = env!("CARGO_BIN_EXE_overwatch");

    // Materialise the store dir with one finding.
    let out = Command::new(bin)
        .args([
            "record-finding",
            "--finding-id",
            "F-1",
            "--source",
            "s",
            "--summary",
            "x",
            "--verdict",
            "confirmed",
        ])
        .current_dir(proj.path())
        .env("HOME", home.path())
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    std::env::set_var("HOME", home.path());
    let findings = overwatch::store::review_findings_path(proj.path()).unwrap();
    let fifo = findings.parent().unwrap().join("dispositions.jsonl");

    let mk = Command::new("mkfifo").arg(&fifo).status().unwrap();
    assert!(mk.success(), "mkfifo failed");

    // Writer: 1st open -> empty (valid, Known(empty)); later opens -> garbage.
    let fifo_w = fifo.clone();
    std::thread::spawn(move || {
        let mut n = 0usize;
        loop {
            // Blocks until a reader opens the pipe.
            let Ok(mut f) = std::fs::OpenOptions::new().write(true).open(&fifo_w) else {
                return;
            };
            if n > 0 {
                let _ = f.write_all(b"{undecodable-mid-run\n");
            }
            drop(f);
            n += 1;
            std::thread::sleep(Duration::from_millis(300));
        }
    });

    let out = Command::new(bin)
        .args(["review-metrics", "--json"])
        .current_dir(proj.path())
        .env("HOME", home.path())
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!(
            "json: {e}: {stdout} / {}",
            String::from_utf8_lossy(&out.stderr)
        )
    });
    assert!(
        v["undetermined_sources"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s == "stale-undisposed join"),
        "the stale-undisposed join failed mid-run but was not reported: {stdout}"
    );
    assert!(
        v["stale_undisposed_with_fix_commit"].is_null(),
        "an uncomputable stale count must be null, never a number: {stdout}"
    );
    assert_eq!(
        out.status.code(),
        Some(3),
        "SomeUndetermined must exit 3: {stdout}"
    );
}
