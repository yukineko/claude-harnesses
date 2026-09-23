// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! End-to-end tests: drive the real built `budgetguard` binary and assert on
//! exit code + stdout. `budgetguard gate` is a Stop hook — harness errors always
//! exit 0 (never break the turn). The rest is a subcommand CLI.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Isolated temp HOME so `~/.budgetguard` state/ledger is never read or written.
fn temp_home(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "budgetguard-it-{}-{}-{}",
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

/// Run `budgetguard <args>` with `payload` on stdin under an isolated HOME/CWD.
fn run(args: &[&str], payload: &str, home: &PathBuf) -> (i32, String) {
    let bin = env!("CARGO_BIN_EXE_budgetguard");
    let mut child = Command::new(bin)
        .args(args)
        .env("HOME", home)
        .current_dir(home)
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
fn version_flag_exits_zero() {
    let home = temp_home("version");
    let (code, stdout) = run(&["--version"], "", &home);
    assert_eq!(code, 0, "--version must exit 0");
    assert!(
        stdout.contains("budgetguard"),
        "version output should name the binary, got: {stdout}"
    );
}

#[test]
fn status_prints_spend_summary() {
    // Read-only: prints resolved config + today's spend from an empty ledger.
    let home = temp_home("status");
    let (code, stdout) = run(&["status"], "", &home);
    assert_eq!(code, 0, "status must exit 0");
    assert!(
        stdout.contains("enabled:") && stdout.contains("today ("),
        "status should print config + today's spend, got: {stdout}"
    );
}

#[test]
fn status_json_emits_machine_readable_pressure() {
    let home = temp_home("status-json");
    let (code, stdout) = run(&["status", "--json"], "", &home);
    assert_eq!(code, 0);
    assert!(
        stdout.contains("\"day_usd\"") && stdout.contains("\"pressure\""),
        "--json should emit day_usd/pressure, got: {stdout}"
    );
}

#[test]
fn gate_with_empty_fields_exits_zero() {
    // A valid Stop payload but with empty transcript_path/session_id → the gate
    // returns early. The invariant is exit 0, never blocking the turn.
    let home = temp_home("gate-valid");
    let payload = r#"{"hook_event_name":"Stop","session_id":"","transcript_path":""}"#;
    let (code, stdout) = run(&["gate"], payload, &home);
    assert_eq!(code, 0, "gate hook must always exit 0");
    assert!(stdout.trim().is_empty(), "got: {stdout}");
}

#[test]
fn gate_with_malformed_stdin_exits_zero() {
    // Fail-soft invariant: unparseable stdin → early return, never breaks a turn.
    let home = temp_home("gate-bad");
    let (code, stdout) = run(&["gate"], "not json", &home);
    assert_eq!(code, 0, "malformed stdin must still exit 0");
    assert!(stdout.trim().is_empty(), "got: {stdout}");
}
