#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro: `overwatch end --status bogus` is accepted: the lease is
//! released and an Ended event with the free-text status is recorded.

use std::process::{Command, Output};
use tempfile::TempDir;

fn run(home: &TempDir, project: &TempDir, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_overwatch"))
        .args(args)
        .current_dir(project.path())
        .env("HOME", home.path())
        .output()
        .expect("spawn overwatch")
}

#[test]
fn end_with_unknown_status_is_rejected() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let b = run(&home, &project, &["begin", "--key", "k1", "--title", "t"]);
    assert!(b.status.success(), "apparatus: begin failed");
    let e = run(
        &home,
        &project,
        &["end", "--key", "k1", "--status", "bogus-status-zz"],
    );
    assert!(
        !e.status.success(),
        "unknown --status accepted and lease released. stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&e.stdout),
        String::from_utf8_lossy(&e.stderr)
    );
}
