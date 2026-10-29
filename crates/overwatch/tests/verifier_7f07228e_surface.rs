#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Independent verifier test (task 7f07228e): a raw gate signal recorded via the
//! library `store::record_finding` must remain VISIBLE in `review-queue`
//! marked UNVERIFIED (not silently dropped), and a verifier can still record
//! the same id CONFIRMED through the CLI.
use std::fs;
use std::process::Command;

#[test]
fn library_signal_is_visible_and_marked_unverified() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let project = root.path().join("project");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&project).unwrap();
    std::env::set_var("HOME", &home);
    let run = |args: &[&str]| {
        harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_overwatch")))
            .args(args)
            .current_dir(&project)
            .env("HOME", &home)
            .output()
            .unwrap()
    };

    overwatch::store::record_finding(
        &project,
        "gate-exec:run-9:t9".into(),
        "condukt-gate".into(),
        Some("high".into()),
        "gate-check escalated: task t9 risk=high".into(),
        None,
        None,
    )
    .unwrap();

    let q = run(&["review-queue", "--json"]);
    let out = String::from_utf8_lossy(&q.stdout);
    assert!(q.status.success(), "review-queue: {q:?}");
    assert!(
        out.contains("gate-exec:run-9:t9"),
        "row must stay visible: {out}"
    );
    assert!(out.contains("[UNVERIFIED]"), "row must be marked: {out}");

    // Escalation path: a verifier confirms the same id via the CLI.
    let r = run(&[
        "record-finding",
        "--finding-id",
        "gate-exec:run-9:t9",
        "--source",
        "verifier",
        "--severity",
        "high",
        "--summary",
        "gate-check escalated: task t9 risk=high",
        "--verdict",
        "confirmed",
    ]);
    assert!(r.status.success(), "record-finding: {r:?}");
}
