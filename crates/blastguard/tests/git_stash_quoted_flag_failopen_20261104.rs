// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED reproduction of a fail-open (observed 2026-11-04 on main fac6c1e0).
//!
//! `git stash push -u` / `--all` stash and REMOVE untracked files (like
//! `git clean`), so blastguard refuses the unquoted form. After shell quote
//! removal `"-u"`, `--"all"`, `\-u` are the very same argv words, yet
//! blastguard allows them: the flag check sees the quoted/escaped spelling.
//!
//! The scanner now judges the dequoted word, so these cases run by default.

#[path = "support/bounded.rs"]
mod bounded;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn fresh_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "blastguard-stashquoted-{}-{tag}",
        std::process::id()
    ));
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

fn assert_not_allowed(tag: &str, command: &str) {
    let dir = fresh_dir(tag);
    let (d, r) = verdict(&dir, command);
    assert_ne!(
        d, "allow",
        "fail-open: {command:?} dequotes to an untracked-removing stash flag but was allowed ({r})"
    );
}

#[test]
fn double_quoted_dash_u_is_not_allowed() {
    assert_not_allowed("dq-u", r#"git stash push "-u""#);
}

#[test]
fn partially_quoted_dash_dash_all_is_not_allowed() {
    assert_not_allowed("pq-all", r#"git stash push --"all""#);
}

#[test]
fn backslash_escaped_dash_u_is_not_allowed() {
    assert_not_allowed("bs-u", r"git stash push \-u");
}

#[test]
fn backslash_escaped_dash_u_after_message_is_not_allowed() {
    assert_not_allowed("msg-bs-u", r"git stash push -m x \-u");
}

/// Control: the unquoted flag is already refused.
#[test]
fn control_unquoted_dash_u_is_not_allowed() {
    assert_not_allowed("plain-u", "git stash push -u");
}

/// Control: a plain message-only stash stays allowed (the RED cases must not
/// be satisfied by refusing every `git stash push`).
#[test]
fn control_plain_stash_with_message_is_allowed() {
    let dir = fresh_dir("plain-msg");
    let command = r#"git stash push -m "msg""#;
    let (d, r) = verdict(&dir, command);
    assert_eq!(d, "allow", "{command:?} must stay allowed, got {d}: {r}");
}
