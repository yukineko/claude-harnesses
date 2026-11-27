#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Backlog 8f8acf5e, user ruling 2026-11-21: a sudo/doas shell mode (`-s`,
//! `-i`, `--shell`, `--login`) followed by a command word takes the stricter
//! of today's Ask and the verdict of the joined words judged as a shell body.
//! Harmful body -> Deny; harmless body (`sudo -s 'ls -la'`) stays Ask (main) /
//! Deny (subagent, where an Ask resolves to Deny).
//!
//! DOES NOT PROVE: sudo's metacharacter escaping of the words (not modelled,
//! the restrictive reading is used); shapes not listed here.

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
        "bg-sudomax-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn run_as(command: &str, subagent: bool) -> (String, String) {
    let home = unique_home("t");
    let mut p = serde_json::json!({
        "session_id": "sudo-shell-max-20261127",
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

fn expect(command: &str, subagent: bool, want: &str) {
    let (d, r) = run_as(command, subagent);
    assert_eq!(
        d, want,
        "{command:?} (subagent={subagent}) must be {want}, got {d} | {r}"
    );
}

#[test]
fn harmful_body_is_deny_for_every_shell_flag() {
    for flag in ["-s", "-i", "--shell", "--login"] {
        let c = format!("sudo {flag} 'rm -rf /etc'");
        expect(&c, false, "deny");
        expect(&c, true, "deny");
    }
}

#[test]
fn doas_harmful_body_is_deny() {
    expect("doas -s 'rm -rf /etc'", false, "deny");
    expect("doas -s 'rm -rf /etc'", true, "deny");
}

#[test]
fn harmless_body_stays_ask_in_main_and_deny_in_subagent() {
    expect("sudo -s 'ls -la'", false, "ask");
    expect("sudo -s 'ls -la'", true, "deny");
}
