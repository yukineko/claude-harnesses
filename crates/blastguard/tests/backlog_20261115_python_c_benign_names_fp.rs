#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (false positive, existing behaviour, observed 2026-11-15 by the main
//! session on backlog 2e5e080c): benign `python3 -c` programs that merely
//! contain a name resembling a stdin/exec entry point are not Allow.
//! Observed (main / subagent), P = `echo 'print(1)' | `:
//!   P python3 -c 'import re; re.compile("x")'          ask / deny
//!   P python3 -c 'print(globals()["__name__"])'        ask / deny
//! `re.compile` compiles a regex and does not read stdin; the second program
//! only prints a module-global name and does not read stdin either.
//!
//! Two readings: (1) false positive: the gate matches a name/substring
//! (e.g. `compile`, `globals`) as if it were an executing or stdin-reading
//! call, regardless of receiver or context; (2) intended: any use of such names
//! inside `-c` is deliberately judged undetermined, because `compile`/`globals`
//! can be part of a dynamic-execution chain. How to tell: run each command for
//! real against a sandbox and observe that stdin is never read or executed
//! (stdin is only data), and inspect the rule that fires; if it fires on the
//! bare name rather than on a call that executes stdin, reading (1) holds
//! (gate FP = bug).
//!
//! Controls that must stay Deny in both modes (harmful program on stdin
//! actually executed): `python3 -c 'exec(input())'` and `python3 -m asyncio`.
//!
//! DOES NOT PROVE: that other benign names (`eval` on a literal, `getattr`,
//! `__import__`) are Allow, that `compile(input(), ...)` or
//! `globals()[...]` chains that do reach stdin stay judged, or anything about
//! other interpreters.
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
        "bg-pycfp-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "python-c-benign-names-fp-20261115",
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

const BENIGN: &str = "echo 'print(1)' | ";
const HARM: &str = "echo 'import os; os.chmod(\"/etc/hosts\", 0o777)' | ";

/// Commands whose decision is not "deny" are fine; returns the denied ones.
fn denied(cmds: &[String], tag: &str) -> Vec<String> {
    let home = unique_home(tag);
    let mut bad = Vec::new();
    for cmd in cmds {
        for subagent in [false, true] {
            let (d, r) = run_as(cmd, &home, subagent);
            if d == "deny" {
                bad.push(format!("subagent={subagent} {cmd:?} => {d} ({r})"));
            }
        }
    }
    bad
}

/// Returns the commands that were not denied.
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
#[ignore = "RED: 2026-11-15 python -c benign name FP — remove when fixed"]
fn re_compile_in_python_c_is_not_deny() {
    let bad = denied(
        &[format!("{BENIGN}python3 -c 'import re; re.compile(\"x\")'")],
        "recompile",
    );
    assert!(bad.is_empty(), "expected not Deny:\n{}", bad.join("\n"));
}

#[test]
#[ignore = "RED: 2026-11-15 python -c benign name FP — remove when fixed"]
fn globals_name_lookup_in_python_c_is_not_deny() {
    let bad = denied(
        &[format!(
            "{BENIGN}python3 -c 'print(globals()[\"__name__\"])'"
        )],
        "globals",
    );
    assert!(bad.is_empty(), "expected not Deny:\n{}", bad.join("\n"));
}

#[test]
fn control_harmful_program_to_exec_input_is_deny() {
    let bad = undenied(&[format!("{HARM}python3 -c 'exec(input())'")], "ctl-exec");
    assert!(
        bad.is_empty(),
        "control shapes must stay Deny:\n{}",
        bad.join("\n")
    );
}

#[test]
fn control_harmful_program_to_python_m_asyncio_is_deny() {
    let bad = undenied(&[format!("{HARM}python3 -m asyncio")], "ctl-asyncio");
    assert!(
        bad.is_empty(),
        "control shapes must stay Deny:\n{}",
        bad.join("\n")
    );
}
