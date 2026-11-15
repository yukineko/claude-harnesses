#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (system-harm, fail-open; observed 2026-11-14 on blastguard 0.2.580 at
//! 8032b4ebe by the verifier): the harmful program is bound to fd 3 by a
//! separate `exec` statement and a later shell reads it through a DYNAMIC fd
//! reference. Observed (main AND subagent): allow for
//!   exec 3<<< 'chmod 777 /etc/hosts'; bash <&$((1+2))
//! Suspected cause: detect/payload/exec_fds.rs:867
//! `if !named.is_empty() && mentions_dynamic_fd(&words)` only treats a dynamic
//! fd as unmodelled when a named `{fd}` was opened.
//! Policy (specs/bg-exec-fd-tracking.toml R7 + user ruling): literal system harm
//! is Deny in main and subagent; an undeterminable fd is Ask in main / Deny in
//! subagent, never Allow.
//!
//! DOES NOT PROVE: other fd numbers, `exec 3< file`, here-docs, other shells,
//! or that the suspected line is the only cause.
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
        "bg-execfd-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "exec-fd-dynamic-reader-20261114",
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
fn harm_with_dynamic_fd_reader_is_deny() {
    let home = unique_home("harm");
    for cmd in [
        "exec 3<<< 'chmod 777 /etc/hosts'; bash <&$((1+2))",
        "fd=3; exec 3<<< 'chmod 777 /etc/hosts'; bash <&$fd",
        "exec 3<<< 'chmod 777 /etc/hosts'; bash /dev/fd/$((1+2))",
    ] {
        for subagent in [false, true] {
            let (d, r) = run_as(cmd, &home, subagent);
            assert_eq!(d, "deny", "subagent={subagent}: {cmd:?} => {d} ({r})");
        }
    }
}

#[test]
fn benign_with_dynamic_fd_reader_is_not_allow() {
    let home = unique_home("benign");
    for cmd in [
        "exec 3<<< 'echo hi'; bash <&$((1+2))",
        "fd=3; exec 3<<< 'echo hi'; bash <&$fd",
        "exec 3<<< 'echo hi'; bash /dev/fd/$((1+2))",
    ] {
        let (d, r) = run_as(cmd, &home, false);
        assert_eq!(d, "ask", "main: {cmd:?} => {d} ({r})");
        let (d, r) = run_as(cmd, &home, true);
        assert_eq!(d, "deny", "subagent: {cmd:?} => {d} ({r})");
    }
}
