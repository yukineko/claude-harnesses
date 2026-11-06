// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Pins for backlog debc6df3: a brace list in a git word up to and including
//! the subcommand (`git -C {a,b} status`, `git -c x={a,b} log`) cannot be
//! expanded statically, so a would-be Allow becomes Ask. Braces after the
//! subcommand, and any Deny, are unaffected.

#[path = "support/bounded.rs"]
mod bounded;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn fresh_dir(tag: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("blastguard-headbrace-{}-{tag}", std::process::id()));
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

fn assert_verdict(tag: &str, command: &str, want: &str) {
    let dir = fresh_dir(tag);
    let (d, r) = verdict(&dir, command);
    assert_eq!(d, want, "{command:?} -> {d} ({r}), want {want}");
}

#[test]
fn stash_pop_with_stash_ref_stays_allowed() {
    assert_verdict("pop", "git stash pop stash@{0}", "allow");
}

#[test]
fn brace_after_subcommand_in_log_format_is_unchanged() {
    assert_verdict("fmt", "git log --format={a,b}", "allow");
}

#[test]
fn brace_in_commit_message_after_subcommand_is_unchanged() {
    assert_verdict("msg", "git commit -m \"fix {a,b}\"", "allow");
}

#[test]
fn plain_git_status_is_allowed() {
    assert_verdict("plain", "git status", "allow");
}

#[test]
fn brace_in_dash_capital_c_dir_before_subcommand_asks() {
    assert_verdict("dashC", "git -C {a,b} status", "ask");
}

#[test]
fn brace_in_dash_lower_c_config_before_subcommand_asks() {
    assert_verdict("dashc", "git -c x={a,b} log", "ask");
}

#[test]
fn head_brace_does_not_downgrade_a_deny() {
    assert_verdict("deny-plain", "git -C a stash clear", "deny");
    assert_verdict("deny-brace", "git -C {a,b} stash clear", "deny");
}
