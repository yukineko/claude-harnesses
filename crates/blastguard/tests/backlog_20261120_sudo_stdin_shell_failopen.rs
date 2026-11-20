#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED: a privilege wrapper (`sudo`/`doas`) in front of a shell that reads its
//! program from an unseen redirected file must not make it Allow.
//!
//! Observed 2026-11-20 (main 2f39a1620, blastguard 0.2.784; empty stdout = allow):
//!   allow :: sudo bash < /tmp/p
//!   allow :: bash -c 'sudo bash' < /tmp/p
//!   allow :: doas sh < /tmp/p
//!   allow :: sudo -s < /tmp/p
//!   ask   :: bash < /tmp/p          (control, no wrapper)
//!   deny  :: sudo bash -c 'rm -rf /etc'
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
        "bg-sudostdin-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

/// Run the hook. Returns (decision, reason).
fn run_as(command: &str, home: &Home) -> (String, String) {
    let p = serde_json::json!({
        "session_id": "sudo-stdin-shell-probe-20261120",
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

fn assert_not_allow(tag: &str, command: &str) {
    let home = unique_home(tag);
    let (d, r) = run_as(command, &home);
    assert_ne!(
        d, "allow",
        "{command:?} must be ask/deny, got decision={d} reason={r}"
    );
}

#[test]
#[ignore = "RED(e1bb5f7e): blastguard fail-open 2026-11-20 sudo/doas stdin shell — remove the ignore when fixed"]
fn sudo_bash_stdin() {
    assert_not_allow("r1", "sudo bash < /tmp/p");
}

#[test]
#[ignore = "RED(e1bb5f7e): blastguard fail-open 2026-11-20 sudo/doas stdin shell — remove the ignore when fixed"]
fn bash_c_sudo_bash_stdin() {
    assert_not_allow("r2", "bash -c 'sudo bash' < /tmp/p");
}

#[test]
#[ignore = "RED(e1bb5f7e): blastguard fail-open 2026-11-20 sudo/doas stdin shell — remove the ignore when fixed"]
fn doas_sh_stdin() {
    assert_not_allow("r3", "doas sh < /tmp/p");
}

#[test]
#[ignore = "RED(e1bb5f7e): blastguard fail-open 2026-11-20 sudo/doas stdin shell — remove the ignore when fixed"]
fn sudo_s_stdin() {
    assert_not_allow("r4", "sudo -s < /tmp/p");
}

#[test]
#[ignore = "RED(e1bb5f7e): blastguard fail-open 2026-11-20 sudo/doas stdin shell — remove the ignore when fixed"]
fn sudo_u_root_bash_stdin() {
    assert_not_allow("r5", "sudo -u root bash < /tmp/p");
}

#[test]
fn ctl_bash_stdin_not_allow() {
    assert_not_allow("c1", "bash < /tmp/p");
}

#[test]
fn ctl_sudo_bash_c_rm_etc_denied() {
    let home = unique_home("c2");
    let (d, r) = run_as("sudo bash -c 'rm -rf /etc'", &home);
    assert_eq!(d, "deny", "must be deny, got decision={d} reason={r}");
}

#[test]
fn ctl_sudo_ls_not_deny() {
    let home = unique_home("c3");
    let (d, r) = run_as("sudo ls", &home);
    eprintln!("OBSERVED sudo ls => decision={d} reason={r}");
    assert_ne!(d, "deny", "sudo ls got decision={d} reason={r}");
}
