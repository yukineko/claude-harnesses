#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (system-harm fail-open, p0, observed 2026-11-14 on blastguard 0.2.565 at
//! dc9b00c7b): a shell function whose body deletes a path held in a variable
//! pointing at /etc/hosts is Deny when called directly, but Allow in main AND
//! subagent mode when the call is inside a command substitution. Observed
//! (main / subagent):
//!   f() { rm -f $L; }; L=/etc/hosts; x=$(f)      allow / allow  <- fail-open
//!   f() { rm -f $L; }; L=/etc/hosts; x=`f`       allow / allow  <- fail-open
//!   f() { echo x > $L; }; L=/etc/hosts; f        deny  / deny   (control)
//!   f() { echo x > $L; }; L=/etc/hosts; eval f   deny  / deny   (control)
//!   f() { echo x > $L; }; L=/etc/hosts; command f deny / deny   (control)
//! Suspected cause (not asserted): function-call inlining is not applied
//! inside `$(...)` / backtick bodies.
//!
//! DOES NOT PROVE: nested/recursive functions, `$(…)` in other positions
//! (arguments, redirect targets), or non-rm bodies.
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
        "bg-fnsubst-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "fn-call-cmdsubst-20261114",
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

fn undenied(cmds: &[&str], tag: &str) -> Vec<String> {
    let home = unique_home(tag);
    let mut bad = Vec::new();
    for cmd in cmds {
        for subagent in [false, true] {
            let (d, r) = run_as(cmd, &home, subagent);
            if d != "deny" {
                bad.push(format!("subagent={subagent} {cmd:?} => {d} ({r})"));
            }
        }
    }
    bad
}

#[test]
fn function_call_in_command_substitution_deleting_system_file_is_deny() {
    let bad = undenied(
        &[
            "f() { rm -f $L; }; L=/etc/hosts; x=$(f)",
            "f() { rm -f $L; }; L=/etc/hosts; x=`f`",
        ],
        "red",
    );
    assert!(bad.is_empty(), "expected Deny:\n{}", bad.join("\n"));
}

#[test]
fn control_direct_function_call_writing_system_file_is_deny() {
    let bad = undenied(
        &[
            "f() { echo x > $L; }; L=/etc/hosts; f",
            "f() { echo x > $L; }; L=/etc/hosts; eval f",
            "f() { echo x > $L; }; L=/etc/hosts; command f",
        ],
        "ctl",
    );
    assert!(
        bad.is_empty(),
        "control must stay Deny:\n{}",
        bad.join("\n")
    );
}
