#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Audit B item 17 (memory: flow-bypass-permissions-mode). RED.
//!
//! The user wants every `/flow` run in Bypass permissions mode (2026-08-12:
//! 「flowのときは常にそうして」). Today that rests on the model remembering to ask
//! for Shift+Tab. The planned mechanization: a UserPromptSubmit hook that sees a
//! `/flow` prompt with `permission_mode` other than `bypassPermissions` blocks
//! the prompt, telling the user to switch modes first.
//!
//! Owner: the `flow` plugin is skill-only (no binary, no hooks), so the RED is
//! pinned on autoflow's existing UserPromptSubmit hook (`autoflow
//! prompt-submit`), which is already the /flow-aware prompt hook (it injects
//! the post-/compact "/flow を再開" instruction). If the fix instead ships a new
//! hook in the flow plugin, this test moves with it; the property is the same.
//!
//! Property: for prompt `/flow` and `permission_mode` "auto" or "default", the
//! hook prints a `{"decision":"block"}` JSON line. A missing `permission_mode`
//! is "cannot determine", which CLAUDE.md §3 resolves to block as well.
//! Controls (green today, must stay green): `/flow` under `bypassPermissions`,
//! and a non-/flow prompt under "auto", are not blocked.

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

fn run(home: &Path, payload: serde_json::Value) -> (Option<i32>, String, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_autoflow"));
    cmd.arg("prompt-submit")
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home)
        .current_dir(home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn autoflow");
    let _ = child
        .stdin
        .take()
        .expect("stdin")
        .write_all(payload.to_string().as_bytes());
    let out = child.wait_with_output().expect("autoflow runs");
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn payload(prompt: &str, mode: Option<&str>) -> serde_json::Value {
    let mut v = serde_json::json!({
        "session_id": "audit-b-flow-bypass",
        "transcript_path": "",
        "cwd": "/",
        "hook_event_name": "UserPromptSubmit",
        "prompt": prompt,
    });
    if let Some(m) = mode {
        v["permission_mode"] = serde_json::Value::String(m.to_string());
    }
    v
}

fn blocked(stdout: &str) -> bool {
    stdout
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l.trim()).ok())
        .any(|v| v["decision"] == "block")
}

fn assert_blocked(prompt: &str, mode: Option<&str>) {
    let home = tempfile::TempDir::new().expect("tempdir");
    let (code, stdout, stderr) = run(home.path(), payload(prompt, mode));
    assert!(
        blocked(&stdout),
        "prompt {prompt:?} with permission_mode {mode:?} must be blocked until the session \
         is in bypassPermissions; got exit {code:?} stdout={stdout:?} stderr={stderr:?}"
    );
}

fn assert_not_blocked(prompt: &str, mode: Option<&str>) {
    let home = tempfile::TempDir::new().expect("tempdir");
    let (code, stdout, stderr) = run(home.path(), payload(prompt, mode));
    assert_eq!(code, Some(0), "stderr={stderr:?}");
    assert!(
        !blocked(&stdout),
        "prompt {prompt:?} with permission_mode {mode:?} must not be blocked; stdout={stdout:?}"
    );
}

#[test]
fn flow_prompt_in_auto_mode_is_blocked() {
    assert_blocked("/flow", Some("auto"));
}

#[test]
fn flow_prompt_in_default_mode_is_blocked() {
    assert_blocked("/flow", Some("default"));
}

#[test]
fn flow_prompt_with_unknown_mode_is_blocked() {
    assert_blocked("/flow", None);
}

#[test]
fn control_flow_prompt_in_bypass_mode_is_not_blocked() {
    assert_not_blocked("/flow", Some("bypassPermissions"));
}

#[test]
fn control_ordinary_prompt_in_auto_mode_is_not_blocked() {
    assert_not_blocked("hello", Some("auto"));
}
