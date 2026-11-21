//! Verification tests for backlog ba09708a, written by the condukt
//! orchestrator of run-20261122-020202-63344 (not the implementer of task
//! cba09), 2026-11-22. The RED file `backlog_20261122_ack_pager_env_protected`
//! covers only `ACK_PAGER=sh ack ...`; these cover the forms the done_criteria
//! names but no test observed: ACK_PAGER_COLOR, an undeterminable value, and
//! controls that must stay Allow.
//!
//! DOES NOT PROVE: real ack behaviour (nothing is executed); inherited or
//! exported environments; .ackrc / ACKRC carrying --pager.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "support/bounded.rs"]
mod bounded;

use std::process::{Command, Stdio};

struct Home(std::path::PathBuf);
impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn unique_home(tag: &str) -> Home {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "bg-ackpagerforms-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn decide(command: &str, home: &Home, subagent: bool) -> String {
    let mut p = serde_json::json!({
        "session_id": "ack-pager-env-forms-20261122",
        "cwd": home.0.join("wt"),
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": {"command": command},
    });
    if subagent {
        p["agent_id"] = serde_json::json!("test-subagent");
    }
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_blastguard"));
    cmd.env_clear()
        .env("HOME", &home.0)
        .env("PATH", "/usr/bin:/bin")
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = bounded::run_bounded(&mut cmd, p.to_string().as_bytes(), bounded::HOOK_DEADLINE);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if stdout.trim().is_empty() {
        return "allow".into();
    }
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    v["hookSpecificOutput"]["permissionDecision"]
        .as_str()
        .unwrap_or("?")
        .to_string()
}

fn expect(cmd: &str, tag: &str, want_allow: bool) {
    let home = unique_home(tag);
    let mut bad = Vec::new();
    for subagent in [false, true] {
        let d = decide(cmd, &home, subagent);
        let is_allow = d == "allow";
        if is_allow != want_allow || (!is_allow && d != "ask" && d != "deny") {
            bad.push(format!("subagent={subagent} => {d}"));
        }
    }
    assert!(
        bad.is_empty(),
        "expected {} for {cmd:?}:\n{}",
        if want_allow { "Allow" } else { "not Allow" },
        bad.join("\n")
    );
}

#[test]
fn ack_pager_color_env_prefix_protected_operand_is_not_allow() {
    expect("ACK_PAGER_COLOR=sh ack pat .githooks/pre-commit", "color", false);
}

#[test]
fn ack_pager_undeterminable_value_protected_operand_is_not_allow() {
    expect("ACK_PAGER=\"$X\" ack pat .githooks/pre-commit", "undet", false);
}

#[test]
fn ack_pager_quoted_env_prefix_protected_operand_is_not_allow() {
    expect("ACK_PAGER='sh' ack pat .githooks/pre-commit", "quoted", false);
}

#[test]
fn ack_pager_destructive_value_is_not_allow() {
    expect("ACK_PAGER='rm -rf ~/work' ack pat src", "rmval", false);
}

#[test]
fn control_plain_ack_protected_operand_stays_allow() {
    expect("ack pat .githooks/pre-commit", "plain", true);
}

#[test]
fn control_ack_pager_less_unprotected_operand_stays_allow() {
    expect("ACK_PAGER=less ack pat src", "less", true);
}

#[test]
fn control_unrelated_env_prefix_on_ack_stays_allow() {
    expect("LC_ALL=C ack pat .githooks/pre-commit", "lcall", true);
}
