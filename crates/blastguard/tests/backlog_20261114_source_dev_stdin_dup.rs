#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (candidate fail-open, reported 2026-11-14 at dde31f11b): `source /dev/stdin
//! <&3`, `. /dev/stdin <&3`, `source /dev/stdin <&$fd`, `source /dev/fd/0 <&$fd`
//! were reported allow/allow (main/subagent) while `bash /dev/stdin <&3` is
//! ask/deny.
//!
//! Two readings: (1) real defect: `source`/`.` with a /dev/stdin or /dev/fd/0
//! operand runs a program read from an inherited/unknown fd, the same
//! undeterminable program source as `bash /dev/stdin <&3`, so it must be
//! non-Allow in main and Deny in subagent; (2) intended: `source` is not a stdin
//! reader in blastguard's model. Tell apart: if `bash /dev/stdin <&3` is
//! non-Allow, a `source` of the same operand with the same dup is inconsistent.
//!
//! Asserted: main != allow, subagent == deny, for each of the four commands.
//!
//! DOES NOT PROVE: other operands, the shell's real fd table, or that every
//! `source` is unsafe. Controls: `bash /dev/stdin <&3` (non-Allow main / Deny
//! subagent) and `source ./script.sh` (verdict recorded in the assert message;
//! asserted only to be the same in both modes' sense of not being Deny in main).
//!
//! RED written by an independent test author, not the implementer.

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
        "bg-side1114-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "side-findings-20261114",
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
        return ("allow".into(), String::new());
    }
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let h = &v["hookSpecificOutput"];
    (
        h["permissionDecision"].as_str().unwrap_or("?").to_string(),
        h["permissionDecisionReason"]
            .as_str()
            .unwrap_or("")
            .to_string(),
    )
}

fn check(cmd: &str, tag: &str) {
    let home = unique_home(tag);
    let (m, mr) = run_as(cmd, &home, false);
    let (s, sr) = run_as(cmd, &home, true);
    assert!(
        m != "allow",
        "main must not allow; cmd={cmd:?} got={m} reason={mr}"
    );
    assert_eq!(s, "deny", "subagent must deny; cmd={cmd:?} reason={sr}");
}

#[test]
#[ignore = "RED: 2026-11-14 source /dev/stdin <&3 allowed — remove the ignore when fixed"]
fn source_dev_stdin_dup_literal_fd() {
    check("source /dev/stdin <&3", "a");
}

#[test]
#[ignore = "RED: 2026-11-14 . /dev/stdin <&3 allowed — remove the ignore when fixed"]
fn dot_dev_stdin_dup_literal_fd() {
    check(". /dev/stdin <&3", "b");
}

#[test]
#[ignore = "RED: 2026-11-14 source /dev/stdin <&$fd allowed — remove the ignore when fixed"]
fn source_dev_stdin_dup_var_fd() {
    check("source /dev/stdin <&$fd", "c");
}

#[test]
#[ignore = "RED: 2026-11-14 source /dev/fd/0 <&$fd allowed — remove the ignore when fixed"]
fn source_dev_fd0_dup_var_fd() {
    check("source /dev/fd/0 <&$fd", "d");
}

#[test]
fn control_bash_dev_stdin_dup_is_not_allowed() {
    check("bash /dev/stdin <&3", "e");
}

#[test]
fn control_source_plain_script_is_not_denied_in_main() {
    let home = unique_home("f");
    let (m, mr) = run_as("source ./script.sh", &home, false);
    assert_ne!(m, "deny", "control verdict recorded: {m} reason={mr}");
}
