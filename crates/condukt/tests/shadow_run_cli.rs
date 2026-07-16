//! Integration coverage for the opt-in shadow-run CLI (`condukt shadow-run
//! ...`). Exercises the enable/disable/status gate end-to-end against the
//! real binary, and confirms `exec` refuses to fire while disabled — the
//! manual-trigger-only contract that keeps the automatic-detection scope cut
//! (backlog `cb2aabff`).
//!
//! `CONDUKT_SHADOW_RUN_DIR` points the flag file at a per-test tempdir so
//! these never touch the real `~/.condukt`.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_condukt");

fn condukt() -> Command {
    Command::new(BIN)
}

#[test]
fn status_defaults_to_disabled_and_exits_nonzero() {
    let dir = tempfile::tempdir().unwrap();
    let out = condukt()
        .args(["shadow-run", "status"])
        .env("CONDUKT_SHADOW_RUN_DIR", dir.path())
        .output()
        .expect("spawn condukt shadow-run status");
    assert!(
        !out.status.success(),
        "status must exit nonzero when disabled (mirrors autonomy-check's contract)"
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "disabled");
}

#[test]
fn enable_then_status_reports_enabled_and_exits_zero() {
    let dir = tempfile::tempdir().unwrap();
    let enable = condukt()
        .args(["shadow-run", "enable"])
        .env("CONDUKT_SHADOW_RUN_DIR", dir.path())
        .output()
        .expect("spawn condukt shadow-run enable");
    assert!(enable.status.success());

    let status = condukt()
        .args(["shadow-run", "status"])
        .env("CONDUKT_SHADOW_RUN_DIR", dir.path())
        .output()
        .expect("spawn condukt shadow-run status");
    assert!(status.status.success(), "status must exit 0 when enabled");
    assert_eq!(String::from_utf8_lossy(&status.stdout).trim(), "enabled");
}

#[test]
fn exec_refuses_to_fire_while_disabled() {
    let dir = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    Command::new("git")
        .args(["init", "-b", "main"])
        .current_dir(repo.path())
        .status()
        .unwrap();
    Command::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(repo.path())
        .status()
        .unwrap();
    Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(repo.path())
        .status()
        .unwrap();
    std::fs::write(repo.path().join("base.txt"), "base\n").unwrap();
    Command::new("git")
        .args(["add", "."])
        .current_dir(repo.path())
        .status()
        .unwrap();
    Command::new("git")
        .args(["commit", "-m", "init"])
        .current_dir(repo.path())
        .status()
        .unwrap();

    let out = condukt()
        .args([
            "shadow-run",
            "exec",
            "--topic",
            "t1-shadow",
            "--branch",
            "shadow/t1-opus",
            "--model",
            "opus",
        ])
        .current_dir(repo.path())
        .env("CONDUKT_SHADOW_RUN_DIR", dir.path())
        .output()
        .expect("spawn condukt shadow-run exec");

    assert!(
        !out.status.success(),
        "exec must refuse to fire while shadow-run is disabled"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("disabled"),
        "error should mention the flag being disabled: {stderr}"
    );
    // No worktree should have been created under the repo's default worktree_base.
    let list = Command::new("git")
        .args(["worktree", "list"])
        .current_dir(repo.path())
        .output()
        .unwrap();
    let listing = String::from_utf8_lossy(&list.stdout);
    assert!(
        !listing.contains("t1-shadow"),
        "no shadow worktree should exist after a refused exec: {listing}"
    );
}

#[test]
fn exec_creates_worktree_when_enabled() {
    let dir = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    for args in [
        vec!["init", "-b", "main"],
        vec!["config", "user.email", "test@example.com"],
        vec!["config", "user.name", "Test"],
    ] {
        Command::new("git")
            .args(&args)
            .current_dir(repo.path())
            .status()
            .unwrap();
    }
    std::fs::write(repo.path().join("base.txt"), "base\n").unwrap();
    Command::new("git")
        .args(["add", "."])
        .current_dir(repo.path())
        .status()
        .unwrap();
    Command::new("git")
        .args(["commit", "-m", "init"])
        .current_dir(repo.path())
        .status()
        .unwrap();

    let enable = condukt()
        .args(["shadow-run", "enable"])
        .env("CONDUKT_SHADOW_RUN_DIR", dir.path())
        .output()
        .unwrap();
    assert!(enable.status.success());

    let worktree_base_dir = tempfile::tempdir().unwrap();
    let worktree_base = worktree_base_dir.path().join("shadow-wt-test-base");
    let exec = condukt()
        .args([
            "shadow-run",
            "exec",
            "--topic",
            "t2-shadow",
            "--branch",
            "shadow/t2-haiku",
            "--model",
            "haiku",
        ])
        .current_dir(repo.path())
        .env("CONDUKT_SHADOW_RUN_DIR", dir.path())
        .env("CONDUKT_WORKTREE_BASE", &worktree_base)
        .output()
        .unwrap();
    assert!(
        exec.status.success(),
        "exec must succeed when enabled: {}",
        String::from_utf8_lossy(&exec.stderr)
    );
    let printed_path = String::from_utf8_lossy(&exec.stdout).trim().to_string();
    assert!(
        std::path::Path::new(&printed_path).exists(),
        "printed worktree path should exist: {printed_path}"
    );

    // finish: discard (never merge) + best-effort fugu-router record.
    let finish = condukt()
        .args([
            "shadow-run",
            "finish",
            "--path",
            &printed_path,
            "--branch",
            "shadow/t2-haiku",
            "--title",
            "t2 shadow attempt",
            "--model",
            "haiku",
            "--pass",
            "--cost",
            "0.05",
            "--duration",
            "3.2",
        ])
        .current_dir(repo.path())
        .env("CONDUKT_SHADOW_RUN_DIR", dir.path())
        .output()
        .unwrap();
    assert!(
        finish.status.success(),
        "finish must succeed even when fugu-router is absent (soft dependency): {}",
        String::from_utf8_lossy(&finish.stderr)
    );
    assert!(
        !std::path::Path::new(&printed_path).exists(),
        "shadow worktree must be discarded after finish"
    );
}
