#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED: `rg --pre CMD pat FILE` runs `CMD FILE`. With a protected operand
//! (.githooks/pre-commit) that must not be Allow.
//!
//! DOES NOT PROVE: real rg behaviour (nothing is executed).

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
        "bg-rgpre-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn decide(command: &str, home: &Home, subagent: bool) -> String {
    let mut p = serde_json::json!({
        "session_id": "rg-pre-protected-20261121",
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

fn expect_not_allow(cmd: &str, tag: &str) {
    let home = unique_home(tag);
    let mut bad = Vec::new();
    for subagent in [false, true] {
        let d = decide(cmd, &home, subagent);
        if d != "ask" && d != "deny" {
            bad.push(format!("subagent={subagent} => {d}"));
        }
    }
    assert!(bad.is_empty(), "expected not Allow for {cmd:?}:\n{}", bad.join("\n"));
}

#[test]
#[ignore = "RED(1f037eb5): rg --pre with a protected operand is Allow"]
fn rg_pre_space_form_protected_operand_is_not_allow() {
    expect_not_allow("rg --pre sh pat .githooks/pre-commit", "space");
}

#[test]
#[ignore = "RED(1f037eb5): rg --pre with a protected operand is Allow"]
fn rg_pre_equals_form_protected_operand_is_not_allow() {
    expect_not_allow("rg --pre=sh pat .githooks/pre-commit", "eq");
}
