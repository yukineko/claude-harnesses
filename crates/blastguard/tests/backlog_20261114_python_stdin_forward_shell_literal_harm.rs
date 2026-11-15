#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (observed 2026-11-14 at main and in the t4 worktree):
//! `echo 'rm -rf /' | python3 -c 'import os; os.system(input())'` and
//! `echo 'rm -rf /' | python3 -c 'import subprocess,sys;
//! subprocess.run(sys.stdin.read(), shell=True)'` are allow in main (reported
//! allow in subagent mode too).
//!
//! Two readings: (1) real fail-open: the python program forwards its stdin to a
//! shell and the upstream literal on the command line is system harm; (2)
//! intended limit: python program semantics are not tracked, so allow. Tell
//! apart: the standing ruling (system harm is Deny through any wrapper, main
//! and subagent) decides it; the control shows harmless forwarded stdin is
//! allow.
//!
//! Asserted: deny in both main and subagent mode.
//!
//! DOES NOT PROVE: other harm literals, other interpreters, non-literal
//! upstream text. The control asserts only that harmless stdin printed by
//! python is allow.
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
        "bg-side1114r2-{tag}-{}-{}",
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

#[test]
#[ignore = "RED: 2026-11-14 python os.system(input()) forwards stdin 'rm -rf /' to a shell and is allow — remove the ignore when fixed"]
fn f10_python_os_system_input_forwarding_harm_is_deny_in_both_modes() {
    let home = unique_home("f10a");
    let cmd = "echo 'rm -rf /' | python3 -c 'import os; os.system(input())'";
    let (m, mr) = run_as(cmd, &home, false);
    let (s, sr) = run_as(cmd, &home, true);
    assert_eq!(m, "deny", "main reason={mr}");
    assert_eq!(s, "deny", "subagent reason={sr}");
}

#[test]
#[ignore = "RED: 2026-11-14 python subprocess.run(sys.stdin.read(), shell=True) forwards stdin 'rm -rf /' and is allow — remove the ignore when fixed"]
fn f10_python_subprocess_stdin_forwarding_harm_is_deny_in_both_modes() {
    let home = unique_home("f10b");
    let cmd = "echo 'rm -rf /' | python3 -c 'import subprocess,sys; subprocess.run(sys.stdin.read(), shell=True)'";
    let (m, mr) = run_as(cmd, &home, false);
    let (s, sr) = run_as(cmd, &home, true);
    assert_eq!(m, "deny", "main reason={mr}");
    assert_eq!(s, "deny", "subagent reason={sr}");
}

#[test]
fn f10_control_harmless_stdin_print_is_allow() {
    let home = unique_home("f10c");
    let (d, r) = run_as(
        "echo hi | python3 -c 'import sys; print(sys.stdin.read())'",
        &home,
        false,
    );
    assert_eq!(d, "allow", "reason={r}");
}
