#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (fail-open, observed 2026-11-14 at main 576d0747): python write APIs
//! whose system-path target is not a single literal argument of the call
//! are Allow:
//! `(Path('/etc') / 'hosts').write_text('x')`,
//! `Path('/etc').joinpath('hosts').write_text('x')`,
//! `shutil.copy(dst='/etc/hosts', src='/tmp/x')` (keyword arguments),
//! `from shutil import copy as c; c('/tmp/x','/etc/hosts')` (aliased import),
//! `Path('/tmp/y').replace('/etc/hosts')` (destination operand).
//! The 569b9b9d fix denies the direct shapes (`Path('/etc/hosts').write_text`,
//! `shutil.copy(src,'/etc/hosts')`); these residuals were found by its
//! independent verifier and are Allow before that fix as well (main 33464587).
//! RED written by the flow-8cca7ef5 orchestrator, a non-implementer.
//!
//! Asserted: no listed program is Allow, in the main session and in a
//! subagent. Control (not ignored): `os.chmod('/etc/hosts',0)` is denied.
//!
//! DOES NOT PROVE: shapes beyond those listed (variables, f-strings, other
//! path builders) are judged; nor what the decision reason says.

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
        "bg-pwr-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

/// Run the hook. Returns (decision, reason).
fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "python-write-residual-20261114",
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

const SHAPES: &[&str] = &[
    "python3 -c \"from pathlib import Path; (Path('/etc') / 'hosts').write_text('x')\"",
    "python3 -c \"from pathlib import Path; Path('/etc').joinpath('hosts').write_text('x')\"",
    "python3 -c \"import shutil; shutil.copy(dst='/etc/hosts', src='/tmp/x')\"",
    "python3 -c \"from shutil import copy as c; c('/tmp/x','/etc/hosts')\"",
    "python3 -c \"from pathlib import Path; Path('/tmp/y').replace('/etc/hosts')\"",
];

const CONTROL: &str = "python3 -c \"import os; os.chmod('/etc/hosts',0)\"";

#[test]
fn indirect_system_path_targets_of_write_apis_are_not_allowed() {
    let home = unique_home("red");
    let mut allowed = Vec::new();
    for c in SHAPES {
        for subagent in [false, true] {
            let (d, _) = run_as(c, &home, subagent);
            if d == "allow" {
                allowed.push(format!("subagent={subagent} {c}"));
            }
        }
    }
    assert!(
        allowed.is_empty(),
        "{} of {} (shape x mode) indirect system-path writes were ALLOWED:\n{}",
        allowed.len(),
        SHAPES.len() * 2,
        allowed.join("\n")
    );
}

#[test]
fn control_plain_os_chmod_on_system_path_is_denied() {
    let home = unique_home("ctl");
    for subagent in [false, true] {
        let (d, r) = run_as(CONTROL, &home, subagent);
        assert_eq!(d, "deny", "control subagent={subagent}: {d} {r}");
    }
}
