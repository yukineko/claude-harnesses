#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (system-harm fail-open, existing behaviour, observed 2026-11-14 by a
//! verifier on a task worktree; side finding of 6f4ec5ce): a program that makes
//! /etc/hosts world-writable, fed on stdin to python3 in a mode that still
//! executes stdin, is Allow in main and subagent mode.
//! Observed (main / subagent), P = `echo 'import os; os.chmod("/etc/hosts", 0o777)' | `:
//!   P python3                                          deny  / deny  (control)
//!   P python3 -m asyncio                               allow / allow <- fail-open
//!   P PYTHONINSPECT=1 python3 script.py                allow / allow <- fail-open
//!   P python3 -c 'import code; code.interact()'        allow / allow <- fail-open
//! The asyncio REPL executes piped stdin (`echo 'print(12345)' | /usr/bin/python3
//! -m asyncio` printed `>>> 12345`); PYTHONINSPECT=1 makes python read stdin as
//! interactive after the script; `code.interact()` runs an interactive console
//! on stdin.
//!
//! Two readings: (1) real fail-open: the gate judges stdin as executed only for
//! a known list of stdin-reading flags/modules and misses these entry points;
//! (2) intended: stdin is deliberately judged only for those known flags. How to
//! tell: run each command for real against a sandbox target (not /etc/hosts) and
//! observe whether the piped program executes; if it does, reading (1) holds
//! and system harm must be Deny (user ruling: Deny in main and subagent).
//!
//! DOES NOT PROVE: other REPL entry points (`-m pdb`, `-m idlelib`, ipython),
//! other interpreters, or that `python3 script.py` without PYTHONINSPECT stays
//! Allow (stdin is then only data).
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
        "bg-pyrepl-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "pipe-python-repl-variants-20261114",
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
#[ignore = "RED: 2026-11-14 harmful program piped to python3 -m asyncio is Allow — remove the ignore when fixed"]
fn harmful_program_piped_to_python_m_asyncio_is_deny() {
    let bad = undenied(&[format!("{PY}python3 -m asyncio")], "asyncio");
    assert!(bad.is_empty(), "expected Deny:\n{}", bad.join("\n"));
}

#[test]
#[ignore = "RED: 2026-11-14 harmful program piped to PYTHONINSPECT=1 python3 script.py is Allow — remove the ignore when fixed"]
fn harmful_program_piped_to_pythoninspect_script_is_deny() {
    let bad = undenied(
        &[format!("{PY}PYTHONINSPECT=1 python3 script.py")],
        "inspect",
    );
    assert!(bad.is_empty(), "expected Deny:\n{}", bad.join("\n"));
}

#[test]
#[ignore = "RED: 2026-11-14 harmful program piped to python3 -c code.interact() is Allow — remove the ignore when fixed"]
fn harmful_program_piped_to_code_interact_is_deny() {
    let bad = undenied(
        &[format!("{PY}python3 -c 'import code; code.interact()'")],
        "interact",
    );
    assert!(bad.is_empty(), "expected Deny:\n{}", bad.join("\n"));
}

#[test]
fn control_harmful_program_piped_to_bare_python_is_deny() {
    let bad = undenied(&[format!("{PY}python3")], "ctl");
    assert!(
        bad.is_empty(),
        "control shapes must stay Deny:\n{}",
        bad.join("\n")
    );
}
