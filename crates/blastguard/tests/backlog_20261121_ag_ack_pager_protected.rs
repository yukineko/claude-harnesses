#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED: `ag --pager CMD pat FILE` / `ack --pager=CMD pat FILE` with a protected
//! operand (.githooks/pre-commit) must not be Allow.
//!
//! The doc on `argv_is_data_only` (detect/readonly.rs) lists `ag`/`ack --pager`
//! among commands that "can execute an argument", yet only rg `--pre` got a
//! rule (1f037eb5). Two readings:
//!   (a) defect: the pager program is attacker-chosen and runs as a subprocess
//!       of a read-only-looking verb, so the protected-operand rule
//!       (`unknown_verb_protected_ask`) should not exempt it.
//!   (b) intended: the pager receives the match OUTPUT, not the protected file,
//!       so there is no protected-file effect and Allow is right.
//! Telling them apart: run real `ag --pager "sh -c 'touch /tmp/x; cat'" pat f`
//! and check whether the pager receives the file (effect on f) or only the
//! formatted output; ag/ack were not installed where this was written, so the
//! real behaviour was NOT observed. This test pins reading (a): the doc's own
//! claim that the pager "can execute an argument".
//!
//! DOES NOT PROVE: real ag/ack behaviour (nothing is executed).

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
        "bg-agpager-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn decide(command: &str, home: &Home, subagent: bool) -> String {
    let mut p = serde_json::json!({
        "session_id": "ag-pager-protected-20261121",
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
#[ignore = "RED(ag-pager-protected): ag --pager CMD pat <protected> is Allow; doc says pager can execute an argument"]
fn ag_pager_space_form_protected_operand_is_not_allow() {
    expect_not_allow("ag --pager sh pat .githooks/pre-commit", "ag");
}

#[test]
#[ignore = "RED(ack-pager-protected): ack --pager=CMD pat <protected> is Allow; doc says pager can execute an argument"]
fn ack_pager_equals_form_protected_operand_is_not_allow() {
    expect_not_allow("ack --pager=sh pat .githooks/pre-commit", "ack");
}
