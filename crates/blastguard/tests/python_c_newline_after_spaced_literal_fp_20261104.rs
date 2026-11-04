// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! INTENTIONALLY RED reproduction (found while verifying backlog 1943843c).
//!
//! `python3 -c "x='recursive rm'<newline>y=a[:1]"` is a read-only program with
//! no sink; 'recursive rm' is string data. blastguard denies it as "rm with a
//! wildcard": the argv is split on all whitespace, the newline is lost, and the
//! shell pass reads one statement whose command word is `rm'`.

#[path = "support/bounded.rs"]
mod bounded;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn fresh_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("blastguard-pynl-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir
}

fn verdict(cwd: &Path, command: &str) -> (String, String) {
    let bin = env!("CARGO_BIN_EXE_blastguard");
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "cwd": cwd.to_string_lossy(),
        "tool_input": { "command": command },
    })
    .to_string();
    let mut hook = Command::new(bin);
    hook.current_dir(cwd)
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env("CLAUDE_PROJECT_DIR", cwd)
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = bounded::run_bounded(&mut hook, payload.as_bytes(), bounded::HOOK_DEADLINE);
    assert_eq!(
        out.status.code(),
        Some(0),
        "hook must exit 0 (cmd: {command:?}); stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if stdout.trim().is_empty() {
        return ("allow".to_string(), String::new());
    }
    let doc: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout is not JSON for {command:?} ({e}): {stdout}"));
    let hso = doc.get("hookSpecificOutput");
    let decision = hso
        .and_then(|h| h.get("permissionDecision"))
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("no permissionDecision for {command:?}, got: {stdout}"))
        .to_string();
    let reason = hso
        .and_then(|h| h.get("permissionDecisionReason"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    (decision, reason)
}

/// RED: read-only program, spaced literal naming `rm`, newline-separated next
/// statement. Must not be deny.
#[test]
fn python_c_newline_after_spaced_rm_literal_is_not_denied() {
    let dir = fresh_dir("red");
    let cmd = "python3 -c \"x='recursive rm'\ny=a[:1]\"";
    let (d, r) = verdict(&dir, cmd);
    assert_ne!(
        d, "deny",
        "newline-after-spaced-literal FP: -> {d} (reason: {r})"
    );
}

/// Control (must pass): same program with a real sink stays non-allow.
#[test]
fn control_python_c_with_real_rm_rf_sink_stays_non_allow() {
    let dir = fresh_dir("ctl");
    let cmd = "python3 -c \"import os\nos.system('rm -rf /x')\"";
    let (d, r) = verdict(&dir, cmd);
    assert_ne!(d, "allow", "control: -> {d} (reason: {r})");
}
