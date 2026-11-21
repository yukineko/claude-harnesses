#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED probe: `ACK_PAGER=sh ack pat <protected>` selects a pager through the
//! environment instead of the `--pager` option that e33849bd (1914ffa47) made
//! an exec flag. ack documents `ACK_PAGER` / `ACK_PAGER_COLOR` as "a pager
//! program ... to which ack will send its output", so the pager is a
//! caller-chosen program fed the matched lines (protected file content), the
//! same hazard as `ack --pager=sh`.
//!
//! Two readings:
//!   (a) defect: an env-prefix pager is the same exec as `--pager`, so the
//!       protected-operand rule must not exempt it.
//!   (b) intended: blastguard does not model program-specific env variables;
//!       a leading assignment is judged elsewhere (injecting-variable rules).
//! Telling them apart: a ruling on whether ACK_PAGER/ACK_PAGER_COLOR (and
//! ACK_OPTIONS / ACKRC carrying `--pager`) are in scope of e33849bd.
//!
//! Observed by the t3 verifier of run-20261122-004934-13005 (2026-11-22,
//! main 56c4f1697). DOES NOT PROVE: real ack behaviour (nothing is executed);
//! `.ackrc` files.
//!
//! Written by the orchestrator, not an implementer.

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
        "bg-ackpagerenv-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn decide(command: &str, home: &Home, subagent: bool) -> String {
    let mut p = serde_json::json!({
        "session_id": "ack-pager-env-20261122",
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
    assert!(
        bad.is_empty(),
        "expected not Allow for {cmd:?}:\n{}",
        bad.join("\n")
    );
}

#[test]
#[ignore = "RED: ACK_PAGER=sh ack pat <protected> is Allow (env-selected pager not modelled)"]
fn ack_pager_env_prefix_protected_operand_is_not_allow() {
    expect_not_allow("ACK_PAGER=sh ack pat .githooks/pre-commit", "prefix");
}

/// Control (observed not Allow on 56c4f1697): the `env` wrapper spelling.
#[test]
fn ack_pager_env_wrapper_protected_operand_is_not_allow() {
    expect_not_allow("env ACK_PAGER=sh ack pat .githooks/pre-commit", "envwrap");
}
