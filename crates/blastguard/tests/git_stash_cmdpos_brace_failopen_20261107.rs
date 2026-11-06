// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED reproduction of a fail-open (verifier finding on the af5dcf5c fix,
//! branch tip 63e70497, 2026-11-07; re-observed at main 65b3d26e, blastguard 0.2.277).
//!
//! The af5dcf5c fix asks on a brace inside a `git stash` argument, but a
//! brace that builds the subcommand word itself is never seen by the stash
//! detector. bash expands:
//!
//! * `git {stash,push} -u`   -> `git stash push -u`   (untracked files removed)
//! * `git {stash,push,-u}`   -> `git stash push -u`
//! * `git {,stash} push -u`  -> `git stash push -u` (empty alternative dropped)
//!
//! Readings: (a) defect — a destructive-command gate must Ask on a brace it
//! cannot expand anywhere before the subcommand; (b) accepted limit of a
//! static argv splitter. User ruling 2026-11-07 (escalation esc-2efa0125,
//! option A) filed it as (a).

#[path = "support/bounded.rs"]
mod bounded;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn fresh_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "blastguard-stashcmdpos-{}-{tag}",
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
        "fail-open: {command:?} brace-expands to an untracked-removing `git stash push -u` but was allowed ({r})"
    );
}

#[test]
fn brace_list_building_stash_push_is_not_allowed() {
    assert_not_allowed("cmd-list", "git {stash,push} -u");
}

#[test]
fn brace_list_including_sweep_flag_is_not_allowed() {
    assert_not_allowed("cmd-list-u", "git {stash,push,-u}");
}

#[test]
fn brace_with_empty_alternative_before_stash_is_not_allowed() {
    assert_not_allowed("cmd-empty", "git {,stash} push -u");
}

/// Control: the unbraced spelling is already refused.
#[test]
fn control_plain_stash_push_u_is_not_allowed() {
    assert_not_allowed("plain-u", "git stash push -u");
}
