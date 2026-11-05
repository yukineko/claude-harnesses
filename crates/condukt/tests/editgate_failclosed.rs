// このファイルは丸ごと integration test なので unwrap/expect を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! RED reproduction: `condukt editgate` (PostToolUse) must fail CLOSED on a panic.
//!
//! Defect (rev aa4f21ee): `Command::Editgate` runs under the local `run_hook`
//! in `src/main.rs`, which `catch_unwind`s and always `exit(0)`s. A panic before
//! the block line is printed leaves stdout empty, which the harness reads as
//! "no block" (fail-open). A verdict-bearing hook must instead surface a
//! `{"decision":"block","reason":...}` naming the crash.
//!
//! Deterministic panic trigger the implementer must add (test-only hook inside
//! `run_editgate`, after stdin parse, before any early return for in-scope
//! payloads): when env var `CONDUKT_TEST_PANIC_EDITGATE=1`, `panic!(..)`.
//! Today the binary ignores the var, so the test is RED on the assertion.

use std::io::Write;
use std::process::{Command, Stdio};

fn run_editgate(payload: &str, panic_trigger: bool) -> (String, Option<i32>) {
    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_condukt"));
    cmd.arg("editgate")
        .current_dir(cwd.path())
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join("cfg"))
        .env("XDG_STATE_HOME", home.path().join("state"))
        .env("XDG_DATA_HOME", home.path().join("data"))
        .env_remove("CONDUKT_DISABLE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if panic_trigger {
        cmd.env("CONDUKT_TEST_PANIC_EDITGATE", "1");
    }
    let mut child = cmd.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        out.status.code(),
    )
}

fn edit_payload(cwd: &str) -> String {
    serde_json::json!({
        "hook_event_name": "PostToolUse",
        "tool_name": "Edit",
        "tool_input": {"file_path": format!("{cwd}/src/lib.rs")},
        "cwd": cwd,
    })
    .to_string()
}

#[test]
fn editgate_panic_must_block_not_exit_silently() {
    let (stdout, _code) = run_editgate(&edit_payload("/nonexistent/proj"), true);
    let line = stdout
        .lines()
        .find(|l| l.trim_start().starts_with('{'))
        .unwrap_or_else(|| {
            panic!("editgate panicked but printed no block JSON (fail-open); stdout={stdout:?}")
        });
    let v: serde_json::Value = serde_json::from_str(line).expect("block line is JSON");
    assert_eq!(v["decision"], "block", "stdout={stdout:?}");
    let reason = v["reason"].as_str().unwrap_or("").to_lowercase();
    assert!(
        reason.contains("panic") || reason.contains("crash"),
        "reason must mention the crash/panic, got {reason:?}"
    );
}

/// Benign payload (a Read tool call is out of scope): silent, exit 0 today.
#[test]
fn editgate_benign_payload_stays_silent() {
    let payload = serde_json::json!({
        "hook_event_name": "PostToolUse",
        "tool_name": "Read",
        "tool_input": {"file_path": "/tmp/x.rs"},
    })
    .to_string();
    let (stdout, code) = run_editgate(&payload, false);
    assert_eq!(stdout.trim(), "", "benign payload must print nothing");
    assert_eq!(code, Some(0));
}
