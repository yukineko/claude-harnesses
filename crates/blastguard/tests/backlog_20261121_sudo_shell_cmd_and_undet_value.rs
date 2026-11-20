#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Side findings of backlog a761b38f (measured 2026-11-21, blastguard 0.2.827,
//! `crates/blastguard/src/detect/ast/b5.rs` `unwrap_at` sudo/doas arm).
//!
//! (1) shell mode (`-s`/`-i`/`--shell`/`--login`) + command word: sudo runs
//!     `$SHELL -c 'CMD'`, but the arm returns `Unwrapped::At(i)` (direct argv).
//! (2) value options (`-u`, `--user`, ...) skip the value word without checking
//!     it is determined.
//!
//! Written by an independent test author.

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
        "bg-sudoshell-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn decide(tag: &str, command: &str) -> (String, String) {
    let home = unique_home(tag);
    let p = serde_json::json!({
        "session_id": "sudo-shell-cmd-probe-20261121",
        "cwd": home.0.join("wt"),
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": {"command": command},
    });
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

fn assert_deny(tag: &str, command: &str) {
    let (d, r) = decide(tag, command);
    assert_eq!(d, "deny", "{command:?} must be deny, got decision={d} reason={r}");
}

// Observation (2) (undetermined value after -u/--user) and the benign control
// `sudo -s 'ls -la'` were measured GREEN (Deny / not Deny) and are not kept.

#[test]
#[ignore = "RED sudo -s 'rm -rf /etc' is Ask (program file named 'rm -rf /etc'), not Deny"]
fn shell_short_s_quoted_cmd_is_deny() {
    assert_deny("s1", "sudo -s 'rm -rf /etc'");
}

#[test]
#[ignore = "RED sudo -i 'rm -rf /etc' is Ask (program file named 'rm -rf /etc'), not Deny"]
fn shell_short_i_quoted_cmd_is_deny() {
    assert_deny("s2", "sudo -i 'rm -rf /etc'");
}

#[test]
#[ignore = "RED sudo --shell 'rm -rf /etc' is Ask (program file named 'rm -rf /etc'), not Deny"]
fn shell_long_shell_quoted_cmd_is_deny() {
    assert_deny("s3", "sudo --shell 'rm -rf /etc'");
}

#[test]
#[ignore = "RED sudo --login 'rm -rf /etc' is Ask (program file named 'rm -rf /etc'), not Deny"]
fn shell_long_login_quoted_cmd_is_deny() {
    assert_deny("s4", "sudo --login 'rm -rf /etc'");
}
