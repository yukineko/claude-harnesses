#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
//! RED: command wrappers (`env`, `command`, `timeout 5`, `nice`, `sudo`) in
//! front of `git push -f origin main` reduce the verdict below the bare form.
//!
//! Ruling: `git push --force|-f` is Ask (main) / Deny (subagent, no human):
//! `high_blast_tier.rs::git_force_push_is_asked`, `backlog_audit_s02.rs`,
//! `raw_token_quoted_option_20261120.rs` (`PUSH_F`). No ruling allows it.
//! Main is pushed by automation, so a force push to main is irreversible on
//! the remote. Runs the real binary in both modes.
#[path = "support/bounded.rs"]
mod bounded;

use std::process::{Command, Stdio};

/// `subagent=false`: interactive cli session (raw Ask is kept).
/// `subagent=true`: no human present (Ask hardens to Deny).
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
        return "allow".into(); // no output == pass-through Allow
    }
    let v: serde_json::Value = serde_json::from_str(s.trim()).expect("hook JSON");
    v["hookSpecificOutput"]["permissionDecision"]
        .as_str()
        .unwrap_or("allow")
        .to_string()
}

fn rank(v: &str) -> u8 {
    match v {
        "allow" => 0,
        "ask" => 1,
        "deny" => 2,
        other => panic!("unknown verdict {other}"),
    }
}

const BARE: [&str; 2] = [
    "git push -f origin main",
    "git push --force origin main",
];
const WRAPPERS: [&str; 5] = ["env", "command", "timeout 5", "nice", "sudo"];

#[test]
fn control_bare_force_push_is_ask_main_and_deny_subagent() {
    for b in BARE {
        assert_eq!(verdict(b, false), "ask", "main: {b}");
        assert_eq!(verdict(b, true), "deny", "subagent: {b}");
    }
}

#[test]
fn control_wrapped_plain_push_stays_allow() {
    for w in WRAPPERS {
        let c = format!("{w} git push origin feature/x");
        assert_eq!(verdict(&c, false), "allow", "over-block: {c}");
    }
}

#[test]
#[ignore = "RED(e21d3035): wrapper before git push -f origin main is weaker than bare form (BACKLOG_ID)"]
fn wrapped_force_push_is_never_weaker_than_bare() {
    let mut bad = Vec::new();
    for b in BARE {
        for w in WRAPPERS {
            let c = format!("{w} {b}");
            for sub in [false, true] {
                let (bv, wv) = (verdict(b, sub), verdict(&c, sub));
                if rank(&wv) < rank(&bv) {
                    bad.push(format!(
                        "{} bare={bv} wrapped={wv}: {c}",
                        if sub { "subagent" } else { "main" }
                    ));
                }
            }
        }
    }
    assert!(bad.is_empty(), "{} weaker than bare:\n{}", bad.len(), bad.join("\n"));
}
