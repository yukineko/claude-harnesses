//! End-to-end tests for the `gauge` binary.
//!
//! `record` is a Stop hook (runs under `run_hook` → must ALWAYS exit 0, never
//! breaking the turn). The rest is an ordinary CLI. Tests that read the store run
//! with an isolated `HOME` so the real `~/.gauge/store` is never touched (the
//! state dir resolves via `dirs::home_dir()`, which honors `$HOME` on Linux).

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// A unique, isolated temp HOME for one test.
fn temp_home(tag: &str) -> PathBuf {
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("gauge-test-{tag}-{pid}-{nanos}"));
    std::fs::create_dir_all(&dir).expect("create temp home");
    dir
}

/// Run the binary with `args` and `payload` on stdin, under an isolated HOME and
/// CWD (so no stray project `gauge.toml` is picked up). Returns (exit_code, stdout).
fn run_in(home: &PathBuf, args: &[&str], payload: &str) -> (i32, String) {
    let bin = env!("CARGO_BIN_EXE_gauge");
    let mut child = harness_core::git_env::scrub(&mut Command::new(bin))
        .args(args)
        .current_dir(home)
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
fn help_lists_record_subcommand() {
    let home = temp_home("help");
    let (code, stdout) = run_in(&home, &["--help"], "");
    assert_eq!(code, 0, "--help must exit 0");
    assert!(
        stdout.contains("record"),
        "expected the record subcommand in --help, got: {stdout}"
    );
}

#[test]
fn record_hook_exits_zero_on_valid_payload() {
    // A Stop payload pointing at a nonexistent transcript: usage::aggregate
    // returns None and the hook records nothing, but must still exit 0.
    let home = temp_home("record-valid");
    let missing = home.join("transcript.jsonl");
    let payload = format!(
        r#"{{"hook_event_name":"Stop","session_id":"S1","transcript_path":"{}","cwd":"{}"}}"#,
        missing.display(),
        home.display()
    );
    let (code, _stdout) = run_in(&home, &["record"], &payload);
    assert_eq!(code, 0, "record (Stop) hook must always exit 0");
}

#[test]
fn record_hook_exits_zero_on_malformed_stdin() {
    // Fail-soft invariant: garbage stdin must never break the turn.
    let home = temp_home("record-bad");
    let (code, _stdout) = run_in(&home, &["record"], "not json");
    assert_eq!(code, 0, "record hook must exit 0 even on malformed stdin");
}

#[test]
fn status_reports_defaults_on_empty_store() {
    // Read-only subcommand against a fresh temp HOME with no config/store.
    let home = temp_home("status");
    let (code, stdout) = run_in(&home, &["status"], "");
    assert_eq!(code, 0, "status must exit 0");
    assert!(
        stdout.contains("sessions:     0"),
        "expected zero recorded sessions, got: {stdout}"
    );
}

#[test]
fn session_reports_none_recorded_on_empty_store() {
    let home = temp_home("session");
    let (code, stdout) = run_in(&home, &["session"], "");
    assert_eq!(code, 0, "session must exit 0 on an empty store");
    assert!(
        stdout.contains("no sessions recorded yet."),
        "expected the empty-store message, got: {stdout}"
    );
}
