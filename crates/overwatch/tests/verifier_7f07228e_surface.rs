#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Independent verifier test (task 7f07228e): a raw gate signal recorded via the
//! library `store::record_finding` must (1) remain VISIBLE in `review-queue`
//! marked UNVERIFIED (not silently dropped), (2) NOT be bridged by
//! `--to-backlog`, and (3) still become bridgeable once a verifier records
//! the same id CONFIRMED through the CLI.
#![cfg(unix)]
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

#[test]
fn library_signal_is_visible_unverified_and_not_bridged_until_confirmed() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let bindir = root.path().join("bin");
    let project = root.path().join("project");
    let log = root.path().join("adds.log");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&bindir).unwrap();
    fs::create_dir_all(&project).unwrap();
    std::env::set_var("HOME", &home);
    let script = bindir.join("backlog");
    fs::write(
        &script,
        "#!/bin/sh\nif [ \"$1\" = add ]; then echo add >> \"$FAKE_LOG\"; fi\nexit 0\n",
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!("{}:{}", bindir.display(), std::env::var("PATH").unwrap());
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_overwatch"))
            .args(args)
            .current_dir(&project)
            .env("HOME", &home)
            .env("PATH", &path)
            .env("FAKE_LOG", &log)
            .output()
            .unwrap()
    };
    let adds = || {
        fs::read_to_string(&log)
            .map(|s| s.lines().count())
            .unwrap_or(0)
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

    let b = run(&["review-queue", "--to-backlog"]);
    assert!(b.status.success(), "bridge: {b:?}");
    assert_eq!(adds(), 0, "unverified library signal must NOT be bridged");

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
    let b = run(&["review-queue", "--to-backlog"]);
    assert!(b.status.success(), "bridge2: {b:?}");
    assert_eq!(adds(), 1, "confirmed re-record must be bridged");
}
