#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (false positive, p1, observed 2026-11-14 on blastguard 0.2.565 at
//! dc9b00c7b): a read-only inline Python program that only json.load()s
//! ~/.claude/plugins/installed_plugins.json and prints it is Ask (main) / Deny
//! (subagent), reason "…a protected gate/config path — changing it changes
//! which hooks and gates run". Observed (main / subagent):
//!   python3 -c "print(open('/tmp/x.json').read())"            allow / allow (control)
//!   python3 -c "import json;d=json.load(open('/Users/yuki/.claude/plugins/installed_plugins.json'));print(d['plugins'])"
//!                                                              ask   / deny  <- FP
//! Two readings: (FP) the program never writes; naming a protected path in a
//! READ is not "changing it". (intended) detect/interpreter.rs documents that
//! "naming it is how it gets there" and deliberately asks on ANY protected path
//! literal in the program text because the decidable question is the name, not
//! the verb. docs/specs/blastguard.md has no explicit ruling either way (grep
//! of the spec for installed_plugins / python3 -c protected-path reads: none).
//! Per the repo ruling "gate FP = bug", this RED asserts Allow for the read.
//!
//! DOES NOT PROVE: that a WRITE to the same path (open(..., 'w')) is gated —
//! it must stay Ask/Deny and is not asserted here.
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
        "bg-pyread-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "python-read-protected-fp-20261114",
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

const READ: &str = "python3 -c \"import json;d=json.load(open('/Users/yuki/.claude/plugins/installed_plugins.json'));print(d['plugins'])\"";

#[test]
#[ignore = "RED: 2026-11-14 read-only python3 -c json.load of installed_plugins.json is Ask/Deny (FP) — remove the ignore when fixed"]
fn read_only_inline_python_of_installed_plugins_json_is_allow() {
    let home = unique_home("red");
    for subagent in [false, true] {
        let (d, r) = run_as(READ, &home, subagent);
        assert_eq!(d, "allow", "subagent={subagent}: {READ:?} => {d} ({r})");
    }
}

#[test]
fn control_read_only_inline_python_of_unprotected_file_is_allow() {
    let home = unique_home("ctl");
    let cmd = "python3 -c \"print(open('/tmp/x.json').read())\"";
    for subagent in [false, true] {
        let (d, r) = run_as(cmd, &home, subagent);
        assert_eq!(d, "allow", "subagent={subagent}: {cmd:?} => {d} ({r})");
    }
}
