// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! End-to-end tests for the reviewgate binary.
//!
//! `review` is the Stop hook: it must ALWAYS exit 0 toward Claude (the
//! `decision` field, not the exit code, is what blocks a stop). The other
//! subcommands are a plain clap CLI; `status` is read-only.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// A unique, isolated temp dir to use as both HOME and CWD so nothing touches
/// the real home directory.
fn temp_home() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("reviewgate-it-{}-{}", std::process::id(), n));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

/// Run `reviewgate <args>` with `payload` on stdin in an isolated HOME/CWD.
/// Returns (exit_code, stdout).
fn run(args: &[&str], payload: &str) -> (i32, String) {
    let bin = env!("CARGO_BIN_EXE_reviewgate");
    let home = temp_home();
    let mut child = Command::new(bin)
        .args(args)
        .current_dir(&home)
        .env("HOME", &home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("binary spawns");
    if let Some(mut child_stdin) = child.stdin.take() {
        let _ = child_stdin.write_all(payload.as_bytes());
    }
    let out = child.wait_with_output().expect("binary runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

#[test]
fn help_describes_the_gate() {
    let (code, stdout) = run(&["--help"], "");
    assert_eq!(code, 0);
    assert!(
        stdout.contains("Code-review gate"),
        "expected about string, got: {stdout}"
    );
}

#[test]
fn status_reports_resolved_config() {
    let (code, stdout) = run(&["status"], "");
    assert_eq!(code, 0);
    assert!(
        stdout.contains("enabled:"),
        "expected status output, got: {stdout}"
    );
}

#[test]
fn review_hook_valid_payload_exits_zero() {
    // A well-formed Stop payload pointing at a non-git temp dir: the gate runs
    // and must exit 0 (never trap a turn).
    let payload =
        r#"{"hook_event_name":"Stop","session_id":"it-session","stop_hook_active":false}"#;
    let (code, _stdout) = run(&["review"], payload);
    assert_eq!(code, 0, "Stop hook must always exit 0");
}

#[test]
fn review_hook_survives_garbage_stdin() {
    let (code, _stdout) = run(&["review"], "not json");
    assert_eq!(
        code, 0,
        "fail-soft: garbage stdin must never break the turn"
    );
}

#[test]
fn review_hook_survives_empty_stdin() {
    let (code, _stdout) = run(&["review"], "");
    assert_eq!(code, 0, "fail-soft: empty stdin must never break the turn");
}
