#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Black-box tests for `bin/sitecraft` (the per-platform launcher), written by
//! an independent test author (CLAUDE.md §2(a)). Modeled on `crates/webqa/bin/webqa`.
//!
//! Contract: with no platform binary next to it, the launcher must NOT behave
//! like a passing gate.
//!   gate, first stop in a session      -> exit 0, stdout JSON decision=block
//!   gate, stop_hook_active re-entry after this gate's own failed stop
//!                                      -> exit 0, NOT block (bounded)
//!   gate, empty stdin                  -> exit 1 (no decision fabricated)
//!   any other subcommand               -> non-zero, empty stdout

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

fn launcher_src() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("bin")
        .join("sitecraft")
}

/// Copy the launcher alone (no platform binary) into a fresh tempdir.
fn setup() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let src = launcher_src();
    assert!(
        src.is_file(),
        "launcher missing: {} does not exist (RED: crates/sitecraft/bin/sitecraft must be created)",
        src.display()
    );
    let td = tempfile::tempdir().unwrap();
    let bin_dir = td.path().join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let dst = bin_dir.join("sitecraft");
    std::fs::copy(&src, &dst).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let home = td.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    (td, dst, home)
}

fn run(launcher: &PathBuf, home: &PathBuf, args: &[&str], stdin: &str) -> Output {
    let mut child = Command::new(launcher)
        .args(args)
        .env("HOME", home)
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn launcher");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

fn payload(session: &str, active: bool) -> String {
    format!(r#"{{"session_id":"{session}","stop_hook_active":{active}}}"#)
}

#[test]
fn launcher_file_exists_and_is_executable() {
    let src = launcher_src();
    assert!(src.is_file(), "launcher missing: {}", src.display());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&src).unwrap().permissions().mode();
        assert!(mode & 0o111 != 0, "launcher not executable: {mode:o}");
    }
}

#[test]
fn gate_without_binary_blocks_on_first_stop() {
    let (_td, l, home) = setup();
    let out = run(&l, &home, &["gate"], &payload("s1", false));
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("stdout must be a JSON decision");
    assert_eq!(v["decision"], "block", "got: {v}");
    assert!(
        v["reason"].as_str().is_some_and(|r| !r.is_empty()),
        "reason required: {v}"
    );
}

#[test]
fn gate_second_consecutive_failure_with_stop_hook_active_does_not_block() {
    let (_td, l, home) = setup();
    let first = run(&l, &home, &["gate"], &payload("s2", false));
    let v: serde_json::Value = serde_json::from_slice(&first.stdout).expect("first must block");
    assert_eq!(v["decision"], "block");
    let second = run(&l, &home, &["gate"], &payload("s2", true));
    assert_eq!(second.status.code(), Some(0));
    let s = String::from_utf8_lossy(&second.stdout);
    assert!(
        !s.contains("\"block\""),
        "second consecutive failure must be allowed, got: {s}"
    );
}

#[test]
fn gate_reentry_without_prior_failure_in_session_still_blocks() {
    let (_td, l, home) = setup();
    let out = run(&l, &home, &["gate"], &payload("s3", true));
    assert_eq!(out.status.code(), Some(0));
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("re-entry with no prior record must block");
    assert_eq!(v["decision"], "block", "got: {v}");
}

#[test]
fn gate_with_empty_stdin_exits_1_without_decision() {
    let (_td, l, home) = setup();
    let out = run(&l, &home, &["gate"], "");
    assert_eq!(out.status.code(), Some(1));
    assert!(
        out.stdout.is_empty(),
        "no decision may be fabricated: {:?}",
        out.stdout
    );
}

#[test]
fn non_gate_subcommands_fail_with_empty_stdout() {
    let (_td, l, home) = setup();
    for args in [&["decide", "check"][..], &["bogus"][..], &[][..]] {
        let out = run(&l, &home, args, "");
        assert!(
            out.status.code().is_some_and(|c| c != 0),
            "{args:?} must exit non-zero, got {:?}",
            out.status
        );
        assert!(
            out.stdout.is_empty(),
            "{args:?} stdout must be empty: {:?}",
            out.stdout
        );
    }
}
