#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro: the SessionEnd reminder is suppressed whenever stdout is piped.
//!
//! `main.rs::session_end` prints only `if !harness_core::hook::is_headless()`,
//! and `is_headless()` is `!stdout.is_terminal()`. Claude Code runs hooks with
//! stdout captured (a pipe), so the reminder for unshipped work can never be
//! shown by the real hook path: "unshipped work" and "nothing to ship" both
//! produce empty output.
//!
//! Asserts the correct behaviour: with unshipped work present and the payload
//! piped on stdin, the hook must emit the reminder.

use std::io::Write;
use std::process::{Command, Stdio};

#[test]
fn session_end_emits_reminder_for_unshipped_work_when_piped() {
    let repo = tempfile::TempDir::new().unwrap();
    let p = repo.path();
    let o = Command::new("git")
        .args(["init", "-q"])
        .current_dir(p)
        .output()
        .unwrap();
    assert!(o.status.success());
    std::fs::write(p.join("dirty.txt"), "uncommitted").unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_ship"))
        .arg("session-end")
        .current_dir(p)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let payload =
        serde_json::json!({ "cwd": p.to_str().unwrap(), "hook_event_name": "SessionEnd" });
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.to_string().as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.trim().is_empty(),
        "unshipped work (uncommitted file) but the SessionEnd hook printed nothing; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}
