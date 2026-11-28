#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
//! RED: `git push` that deletes a remote branch (`:main`, `--delete main`,
//! `-d main`, `:refs/heads/main`) is weaker than a force push to the same
//! branch. Remote deletion is irreversible, so it must be at least Ask (main)
//! and Deny (subagent). Runs the real binary in both modes.
#[path = "support/bounded.rs"]
mod bounded;

use std::process::{Command, Stdio};

fn verdict(cmd: &str, subagent: bool) -> String {
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": { "command": cmd }
    })
    .to_string();
    let mut hook = Command::new(env!("CARGO_BIN_EXE_blastguard"));
    hook.env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_remove("CLAUDE_CODE_SESSION_ID");
    if !subagent {
        hook.env("CLAUDECODE", "1").env("CLAUDE_CODE_ENTRYPOINT", "cli");
    }
    let out = bounded::run_bounded(&mut hook, payload.as_bytes(), bounded::HOOK_DEADLINE);
    assert_eq!(out.status.code(), Some(0), "hook must always exit 0");
    let s = String::from_utf8_lossy(&out.stdout).into_owned();
    if s.trim().is_empty() {
        return "allow".into();
    }
    let v: serde_json::Value = serde_json::from_str(s.trim()).expect("hook JSON");
    v["hookSpecificOutput"]["permissionDecision"]
        .as_str()
        .unwrap_or("allow")
        .to_string()
}

const DELETE_MAIN: [&str; 4] = [
    "git push origin :main",
    "git push origin --delete main",
    "git push origin -d main",
    "git push origin :refs/heads/main",
];

#[test]
fn control_force_push_main_is_ask_main_and_deny_subagent() {
    assert_eq!(verdict("git push --force origin main", false), "ask");
    assert_eq!(verdict("git push --force origin main", true), "deny");
}

#[test]
fn control_plain_push_stays_allow() {
    assert_eq!(verdict("git push origin feature/x", false), "allow");
}

#[test]
#[ignore = "RED(76dd7aea): git push deleting remote main is Allow while force push is Ask/Deny"]
fn delete_remote_main_is_ask_or_deny_in_main_mode() {
    let bad: Vec<String> = DELETE_MAIN
        .iter()
        .filter(|c| !matches!(verdict(c, false).as_str(), "ask" | "deny"))
        .map(|c| format!("main-mode {c} => {}", verdict(c, false)))
        .collect();
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

#[test]
#[ignore = "RED(76dd7aea): git push deleting remote main is Allow while force push is Ask/Deny"]
fn delete_remote_main_is_deny_in_subagent_mode() {
    let bad: Vec<String> = DELETE_MAIN
        .iter()
        .filter(|c| verdict(c, true) != "deny")
        .map(|c| format!("subagent {c} => {}", verdict(c, true)))
        .collect();
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// Recording only: prints the verdict table (run with --nocapture --include-ignored).
#[test]
#[ignore = "measurement table, not a contract"]
fn measure_table() {
    for c in [
        "git push origin :main",
        "git push origin --delete main",
        "git push origin -d main",
        "git push origin :refs/heads/main",
        "git push origin :feature",
        "git push origin --delete feature",
        "git push --force origin main",
        "git push --force origin feature",
    ] {
        println!("TABLE main={} sub={} :: {c}", verdict(c, false), verdict(c, true));
    }
}
