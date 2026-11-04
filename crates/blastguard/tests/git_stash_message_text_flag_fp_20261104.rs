// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! INTENTIONALLY RED reproduction. `git stash push -m "stray cargo fmt --all
//! reformat" -- a/b.rs` carries `--all` only inside the quoted -m message text,
//! yet blastguard refuses it with the "git stash -u/-a removes untracked ..."
//! reason: message text is read as a flag.

#[path = "support/bounded.rs"]
mod bounded;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn fresh_dir(tag: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("blastguard-stashmsg-{}-{tag}", std::process::id()));
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

const UNTRACKED_REASON: &str = "git stash -u/-a removes untracked";

/// RED: `--all` is message text, not a flag.
#[test]
fn all_inside_stash_message_is_not_read_as_a_flag() {
    let dir = fresh_dir("red");
    let (d, r) = verdict(
        &dir,
        r#"git stash push -m "stray cargo fmt --all reformat" -- a/b.rs"#,
    );
    assert!(
        !r.contains(UNTRACKED_REASON),
        "stash message FP: --all inside -m text refused with the untracked-removal reason -> {d}: {r}"
    );
}

/// Control: the plain message is allowed today.
#[test]
fn control_plain_message_stays_allowed() {
    let dir = fresh_dir("plain");
    let (d, r) = verdict(&dir, r#"git stash push -m "plain msg" -- a/b.rs"#);
    assert_eq!(d, "allow", "control: -> {d} ({r})");
}

/// Controls: real -a / -u flags must stay non-allow.
#[test]
fn control_stash_push_all_stays_non_allow() {
    let dir = fresh_dir("all");
    let (d, r) = verdict(&dir, "git stash push --all");
    assert_ne!(d, "allow", "control: -> {d} ({r})");
}

#[test]
fn control_stash_push_u_stays_non_allow() {
    let dir = fresh_dir("u");
    let (d, r) = verdict(&dir, "git stash push -u");
    assert_ne!(d, "allow", "control: -> {d} ({r})");
}
