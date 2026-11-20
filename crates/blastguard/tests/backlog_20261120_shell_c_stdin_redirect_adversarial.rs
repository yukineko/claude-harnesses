#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Adversarial variants for `shell -c 'shell' < file` (stdin program from an
//! unseen redirected file must not be Allow), plus keep-Allow controls.
//!
//! Written by an independent test author, backlog b838630f, 2026-11-20.

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
        "bg-stdinadv-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

/// Run the hook. Returns (decision, reason).
fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "shell-c-stdin-adversarial-probe-20261120",
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

fn assert_not_allow(tag: &str, command: &str) {
    let home = unique_home(tag);
    let (d, r) = run_as(command, &home, false);
    assert_ne!(
        d, "allow",
        "{command:?} must be ask/deny, got decision={d} reason={r}"
    );
}

fn assert_allow(tag: &str, command: &str) {
    let home = unique_home(tag);
    let (d, r) = run_as(command, &home, false);
    assert_eq!(
        d, "allow",
        "{command:?} must stay allow, got decision={d} reason={r}"
    );
}

#[test]
fn bash_c_sh_fd0_redirect() {
    assert_not_allow("a1", "bash -c 'sh' 0< /tmp/p");
}
#[test]
fn bash_c_true_then_bash() {
    assert_not_allow("a2", "bash -c 'true; bash' < /tmp/p");
}
#[test]
fn zsh_c_bash_s() {
    assert_not_allow("a3", "zsh -c 'bash -s' < /tmp/p");
}
#[test]
fn bash_c_bash_dash() {
    assert_not_allow("a4", "bash -c 'bash -' < /tmp/p");
}
#[test]
fn env_bash_c_bash() {
    assert_not_allow("a5", "env bash -c 'bash' < /tmp/p");
}
#[test]
fn bash_c_bash_readwrite_redirect() {
    assert_not_allow("a6", "bash -c 'bash' <> /tmp/p");
}
#[test]
fn bash_c_cat_pipe_bash() {
    assert_not_allow("a7", "bash -c 'cat | bash' < /tmp/p");
}

#[test]
fn ctl_bash_c_echo_stdin_file() {
    assert_allow("k1", "bash -c 'echo hi' < /tmp/f");
}
#[test]
fn ctl_bash_c_cat_stdin_file() {
    assert_allow("k2", "bash -c 'cat' < /tmp/f");
}
#[test]
fn ctl_bash_c_nested_bash_c_echo() {
    assert_allow("k3", "bash -c 'bash -c \"echo hi\"' < /tmp/f");
}
#[test]
fn ctl_bash_c_echo_pipe_bash() {
    assert_allow("k4", "bash -c 'echo hi | bash' < /tmp/f");
}
#[test]
fn ctl_sh_c_wc_stdin_file() {
    assert_allow("k5", "sh -c 'wc -l' < /tmp/f");
}

#[test]
fn system_harm_after_stdin_shell_is_deny() {
    let home = unique_home("h1");
    let cmd = "bash -c 'bash' < /tmp/p; rm -rf /etc";
    let (d, r) = run_as(cmd, &home, false);
    assert_eq!(
        d, "deny",
        "{cmd:?} must be deny, got decision={d} reason={r}"
    );
}
