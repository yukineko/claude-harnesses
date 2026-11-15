#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (system-harm fail-open, observed 2026-11-14 by a verifier on a task
//! worktree): quoted module names in `-m` invocation and dict-key references in
//! `-c` programs are wrongly judged.
//!
//! Observed (main / subagent), P = `echo 'import os; os.chmod("/etc/hosts", 0o777)' | `:
//!   P python3 -m 'asyncio'                                  allow / allow <- fail-open (should be deny)
//!   P python3 -m "asyncio"                                  allow / allow <- fail-open (should be deny)
//!   P python3 -m asyncio.__main__                           allow / allow <- fail-open (should be deny)
//!   cat data.json | python3 -c 'import sys,json; ...'       allow / allow <- suspected FP (should be allow)
//!   git log | python3 -c 'import re,sys; ...'               allow / allow <- suspected FP (should be allow)
//!   cat f | python3 -c 'import sys; code=...'               allow / allow <- suspected FP (should be allow)
//!
//! Quoted module names: program_source.rs:314 compares the raw token, missing
//! quoted variants of known dangerous modules (asyncio.__main__, etc.). The
//! asyncio REPL executes piped stdin when invoked with `-m asyncio` or
//! `-m asyncio.__main__`, even when the module name is quoted in shell syntax.
//!
//! Dict-key mentions: the parser flags `-c` programs with dict keys like
//! "code", "compile", "cmd", "vars", "locals", "globals" as stdin-program
//! readers. This is overly broad: accessing sys.stdin["code"] is data loading,
//! not stdin-program execution. The baseline (main binary) treats these as
//! allow; the suspected FP flagging them as stdin readers is the regression.
//!
//! Two readings: (1) real fail-open: quoted modules that execute stdin are
//! passed through; dict-key access is misclassified. (2) intended: names are
//! only dangerous unquoted, or dict-key access counts as stdin execution. How
//! to tell: (quoted) run the shell command for real in a sandbox and check if
//! asyncio REPL reads stdin; (dict-key) run the commands against a real file
//! and confirm they only load data, never evaluate it. If observed to read
//! stdin or execute code, reading (1) holds and these must be deny (or allow,
//! respectively). If not, reading (2) holds and gate verdict is correct as-is.
//!
//! DOES NOT PROVE: other entry points in asyncio, other quoted module variants,
//! or that the gate is correct for unquoted modules (already covered by the
//! existing test file backlog_20261114_pipe_to_python_stdin_repl_variants.rs).
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
        "bg-pymod-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "python-module-quoted-20261114",
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

fn unallowed(cmds: &[String], tag: &str) -> Vec<String> {
    let home = unique_home(tag);
    let mut bad = Vec::new();
    for cmd in cmds {
        for subagent in [false, true] {
            let (d, r) = run_as(cmd, &home, subagent);
            if d != "allow" {
                bad.push(format!("subagent={subagent} {cmd:?} => {d} ({r})"));
            }
        }
    }
    bad
}

#[test]
fn harmful_program_piped_to_python_m_quoted_asyncio_is_deny() {
    let bad = undenied(&[format!("{PY}python3 -m 'asyncio'")], "m-quote1");
    assert!(bad.is_empty(), "expected Deny:\n{}", bad.join("\n"));
}

#[test]
fn harmful_program_piped_to_python_m_doublequoted_asyncio_is_deny() {
    let bad = undenied(&[format!("{PY}python3 -m \"asyncio\"")], "m-quote2");
    assert!(bad.is_empty(), "expected Deny:\n{}", bad.join("\n"));
}

#[test]
fn harmful_program_piped_to_python_m_asyncio_dunder_main_is_deny() {
    let bad = undenied(&[format!("{PY}python3 -m asyncio.__main__")], "m-dunder");
    assert!(bad.is_empty(), "expected Deny:\n{}", bad.join("\n"));
}

#[test]
fn json_load_from_stdin_with_code_key_is_allow() {
    let bad = unallowed(
        &[
            "cat data.json | python3 -c 'import sys,json; print(json.load(sys.stdin)[\"code\"])'"
                .to_string(),
        ],
        "json-code-key",
    );
    assert!(
        bad.is_empty(),
        "expected Allow (json dict key access is data loading, not stdin execution):\n{}",
        bad.join("\n")
    );
}

#[test]
fn regex_findall_on_stdin_read_is_allow() {
    let bad = unallowed(
        &["git log | python3 -c 'import re,sys; print(re.compile(\"x\").findall(sys.stdin.read()))'".to_string()],
        "regex-stdin",
    );
    assert!(
        bad.is_empty(),
        "expected Allow (read() is data loading, not stdin execution):\n{}",
        bad.join("\n")
    );
}

#[test]
fn stdin_code_variable_assignment_is_allow() {
    let bad = unallowed(
        &["cat f | python3 -c 'import sys; code=sys.stdin.read(); print(len(code))'".to_string()],
        "code-var",
    );
    assert!(
        bad.is_empty(),
        "expected Allow (variable named 'code' is not stdin execution):\n{}",
        bad.join("\n")
    );
}

#[test]
fn control_harmful_program_piped_to_python_m_unquoted_asyncio_is_deny() {
    let bad = undenied(&[format!("{PY}python3 -m asyncio")], "ctl-m");
    assert!(
        bad.is_empty(),
        "control shapes must stay Deny:\n{}",
        bad.join("\n")
    );
}

#[test]
fn control_harmful_program_piped_to_code_interact_is_deny() {
    let bad = undenied(
        &[format!("{PY}python3 -c 'import code; code.interact()'")],
        "ctl-interact",
    );
    assert!(
        bad.is_empty(),
        "control shapes must stay Deny:\n{}",
        bad.join("\n")
    );
}

#[test]
fn control_json_load_from_stdin_without_key_is_allow() {
    let bad = unallowed(
        &["cat data.json | python3 -c 'import sys,json; print(json.load(sys.stdin))'".to_string()],
        "ctl-json",
    );
    assert!(
        bad.is_empty(),
        "control shapes must stay Allow:\n{}",
        bad.join("\n")
    );
}
