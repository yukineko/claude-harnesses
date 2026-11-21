//! RED (false positive; independent test author, 2026-11-22, observed at main
//! 6ae161ee7 after condukt run run-20261122-020202-63344): since ba09708a,
//! `ACK_PAGER= ack pat .githooks/pre-commit` (EMPTY value) is refused for a
//! protected operand, although an empty ACK_PAGER names no pager program, and
//! the sibling env-exec rule (`crates/blastguard/src/detect/executors.rs`
//! `env_exec_var_verdict`: `Some(v) if v.trim().is_empty() => Decision::Allow`)
//! allows empty values of the variables it watches.
//!
//! The `env_selects_pager` docstring (`crates/blastguard/src/detect/readonly.rs`)
//! states the current behaviour on purpose: "`ACK_PAGER=less`, `ACK_PAGER=` and
//! an unresolvable `ACK_PAGER=\"$X\"` all lose the exemption, as every
//! `--pager=…` value does". So this test separates two readings:
//!   (a) defect: an empty value runs nothing; refusing it is a gate FP;
//!   (b) intended: uniform restrictive over-ask for any ACK_PAGER prefix.
//!
//! Passing means: `ACK_PAGER= ack pat .githooks/pre-commit` is Allow for both a
//! main session and a subagent.
//!
//! DOES NOT PROVE: real ack behaviour (nothing is executed; ack is not installed
//! on the filing machine, so ack's handling of an empty ACK_PAGER was NOT
//! observed -- if ack treats an empty value as a pager command, reading (b) wins);
//! `ACK_PAGER=''` / `ACK_PAGER=""` spellings; ACK_PAGER_COLOR=.
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
        "bg-ackpagerempty-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn decide(command: &str, home: &Home, subagent: bool) -> String {
    let mut p = serde_json::json!({
        "session_id": "ack-pager-empty-20261122",
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
#[ignore = "RED: empty ACK_PAGER= prefix drops ack's read-only exemption (FP)"]
fn empty_ack_pager_prefix_protected_operand_is_allow() {
    expect("ACK_PAGER= ack pat .githooks/pre-commit", "empty", true);
}

#[test]
fn control_plain_ack_protected_operand_stays_allow() {
    expect("ack pat .githooks/pre-commit", "plain", true);
}

#[test]
fn control_nonempty_ack_pager_protected_operand_is_not_allow() {
    expect("ACK_PAGER=sh ack pat .githooks/pre-commit", "sh", false);
}
