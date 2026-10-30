//! End-to-end tests: drive the real built `autoflow` binary and assert on exit
//! code + stdout. `autoflow Stop` is a Stop hook — the load-bearing invariant is
//! that it ALWAYS exits 0, never breaking a turn.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// A unique, isolated temp HOME so the hook never reads the real `~/.autoflow`
/// state/config or `~/.session-insights` metrics (keeps the test deterministic).
fn temp_home(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "autoflow-it-{}-{}-{}",
        tag,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Run `autoflow <args>` with `payload` on stdin under an isolated HOME.
/// Returns (exit_code, stdout).
fn run(args: &[&str], payload: &str, home: &PathBuf) -> (i32, String) {
    let bin = env!("CARGO_BIN_EXE_autoflow");
    let mut child = harness_core::git_env::scrub(&mut Command::new(bin))
        .args(args)
        .env("HOME", home)
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
fn help_lists_the_about_line() {
    let home = temp_home("help");
    let (code, stdout) = run(&["--help"], "", &home);
    assert_eq!(code, 0, "--help must exit 0");
    assert!(
        stdout.contains("Session-end auto-flow gate"),
        "expected the about line, got: {stdout}"
    );
}

#[test]
fn valid_stop_payload_with_no_metrics_is_silent() {
    // A real Stop payload with a session id, but an isolated HOME means no
    // session-insights metrics exist → Idle phase, below thresholds → no block.
    let home = temp_home("valid");
    let payload = r#"{"hook_event_name":"Stop","session_id":"it-sess-123","cwd":"/tmp"}"#;
    let (code, stdout) = run(&["stop"], payload, &home);
    assert_eq!(code, 0, "Stop hook must always exit 0");
    assert!(
        stdout.trim().is_empty(),
        "no work done → no block decision, got: {stdout}"
    );
}

#[test]
fn malformed_stdin_exits_zero() {
    // Fail-soft invariant: garbage stdin parses to default (empty session id) and
    // the hook returns early — it must never break the turn.
    let home = temp_home("malformed");
    let (code, stdout) = run(&["stop"], "not json", &home);
    assert_eq!(code, 0, "malformed stdin must still exit 0");
    assert!(stdout.trim().is_empty(), "got: {stdout}");
}

#[test]
fn empty_stdin_exits_zero() {
    let home = temp_home("empty");
    let (code, stdout) = run(&["stop"], "", &home);
    assert_eq!(code, 0, "empty stdin must still exit 0");
    assert!(stdout.trim().is_empty(), "got: {stdout}");
}
