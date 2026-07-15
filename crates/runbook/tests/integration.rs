//! End-to-end tests for the runbook binary.
//!
//! `inject` is the UserPromptSubmit hook (must always exit 0, never break a
//! turn); `list` and `status` are read-only CLI subcommands.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_home() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("runbook-it-{}-{}", std::process::id(), n));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

/// Run `runbook <args>` with `payload` on stdin in an isolated HOME/CWD.
fn run(args: &[&str], payload: &str) -> (i32, String) {
    let bin = env!("CARGO_BIN_EXE_runbook");
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
fn help_describes_procedure_includes() {
    let (code, stdout) = run(&["--help"], "");
    assert_eq!(code, 0);
    assert!(
        stdout.contains("Reusable procedure includes"),
        "expected about string, got: {stdout}"
    );
}

#[test]
fn list_on_empty_store_reports_no_runbooks() {
    let (code, stdout) = run(&["list"], "");
    assert_eq!(code, 0);
    assert!(
        stdout.contains("no runbooks"),
        "expected empty-store hint, got: {stdout}"
    );
}

#[test]
fn inject_hook_survives_valid_payload() {
    // A prompt with no `!macro` resolves to nothing → silent, exit 0.
    let payload =
        r#"{"hook_event_name":"UserPromptSubmit","prompt":"hello world","session_id":"it"}"#;
    let (code, stdout) = run(&["inject"], payload);
    assert_eq!(code, 0, "UserPromptSubmit hook must always exit 0");
    assert!(
        stdout.trim().is_empty(),
        "a prompt with no macro injects nothing, got: {stdout}"
    );
}

#[test]
fn inject_hook_survives_garbage_stdin() {
    let (code, _stdout) = run(&["inject"], "not json");
    assert_eq!(
        code, 0,
        "fail-soft: garbage stdin must never break the turn"
    );
}

#[test]
fn inject_hook_survives_empty_stdin() {
    let (code, _stdout) = run(&["inject"], "");
    assert_eq!(code, 0, "fail-soft: empty stdin must never break the turn");
}

#[test]
fn status_masks_index_token() {
    // index_token is not a secret (see main.rs `mask_display` doc comment),
    // but status() masks it anyway per explicit user instruction. Assert the
    // masked form appears and the full plaintext default does not.
    let (code, stdout) = run(&["status"], "");
    assert_eq!(code, 0);
    assert!(
        stdout.contains("!ru***"),
        "expected masked index_token in status output, got: {stdout}"
    );
    assert!(
        !stdout.contains("!runbooks"),
        "index_token must not appear in plaintext in status output, got: {stdout}"
    );
}
