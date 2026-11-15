#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (system-harm fail-open, existing behaviour, observed 2026-11-14 in the
//! task worktrees of run-20261114-111444-69657, base b0b4ac6e3 family): a
//! program that makes /etc/hosts world-writable, fed on stdin to python3 in a
//! mode that still executes stdin, is Allow in main and subagent mode.
//! Observed (main / subagent):
//!   echo 'import os; os.chmod("/etc/hosts", 0o777)' | python3              deny  / deny  (control)
//!   ... | python3 -i script.py                                              allow / allow <- fail-open
//!   ... | python3 -m code                                                   allow / allow <- fail-open
//! `-i` runs the script and then reads stdin as an interactive interpreter, so
//! the piped program is executed; `-m code` starts `code.interact()`, which
//! executes stdin as an interactive console.
//!
//! Asserted: both shapes are Deny in main and subagent mode (system harm is
//! Deny everywhere, user ruling). Control: the bare `python3` is Deny.
//!
//! DOES NOT PROVE: other interactive entry points (`-m pdb`, `-c 'import
//! code; code.interact()'`, `PYTHONINSPECT=1`), other interpreters, or that
//! `python3 script.py` without `-i` stays Allow (stdin is then only data).
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
        "bg-pyinteractive-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "pipe-python-interactive-20261114",
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

const PY: &str = "echo 'import os; os.chmod(\"/etc/hosts\", 0o777)' | ";

fn undenied(cmds: &[String], tag: &str) -> Vec<String> {
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
fn harmful_program_piped_to_interactive_python_is_deny() {
    let cmds = vec![
        format!("{PY}python3 -i script.py"),
        format!("{PY}python3 -m code"),
    ];
    let bad = undenied(&cmds, "red");
    assert!(
        bad.is_empty(),
        "expected Deny for every shape:\n{}",
        bad.join("\n")
    );
}

#[test]
fn control_harmful_program_piped_to_bare_python_is_deny() {
    let cmds = vec![format!("{PY}python3")];
    let bad = undenied(&cmds, "ctl");
    assert!(
        bad.is_empty(),
        "control shapes must stay Deny:\n{}",
        bad.join("\n")
    );
}
