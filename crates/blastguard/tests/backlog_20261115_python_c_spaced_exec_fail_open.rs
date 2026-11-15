#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (fail-open, side finding observed 2026-11-15 on base 0.2.603):
//! harmful stdin piped into `python3 -c '<prog>'` where `<prog>` builds and
//! runs the stdin text through a spelling that none of the
//! `CODE_BUILDING` markers (`exec(`, `compile(`, `globals()[`, ...) matches,
//! because the markers are space-sensitive substrings of the case-folded text:
//!   echo 'import os; os.chmod("/etc/hosts", 0o777)' | python3 -c 'exec (input())'
//!   echo '<same>' | python3 -c 'compile (input(),"x","exec")'
//!   echo '<same>' | python3 -c 'g=globals; g()["exec"](input())'
//!
//! Observed by the test designer at HEAD: the first two are already deny in
//! main and subagent mode (so those tests are regression guards, not RED);
//! the `g=globals; g()["exec"](input())` form is allow in both modes.
//!
//! Two readings: (1) real defect: the program text is not what executes (it
//! executes the stdin text), so a system-harm stdin must be deny exactly as
//! the control `exec(input())` is; (2) intended: only the listed spellings
//! are covered, an alias is out of scope. Tell apart: the control
//! `exec(input())` with the same stdin is deny, and the user ruling is that
//! system harm is deny through any wrapper, so an alias that reaches the same
//! `exec` is the same defect -> reading (1).
//!
//! Asserted: harmful stdin -> deny in main and subagent mode for each of the
//! three programs. Benign stdin (`print(1)`) is recorded as allow/allow: that
//! is an observation of the space-sensitive markers, NOT a requirement; if a
//! fix makes it ask, update this test deliberately.
//!
//! DOES NOT PROVE: other aliases (`getattr(__builtins__, ...)`), harm other
//! than `os.chmod` on /etc/hosts, reason text, or behaviour with a real
//! interpreter. The control only shows `exec(input())` with harmful stdin is
//! deny.
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
        "bg-side1115-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "side-findings-20261115",
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

const HARM_STDIN: &str = r#"import os; os.chmod("/etc/hosts", 0o777)"#;

const SPACED_EXEC: &str = "exec (input())";
const SPACED_COMPILE: &str = r#"compile (input(),"x","exec")"#;
const GLOBALS_ALIAS: &str = r#"g=globals; g()["exec"](input())"#;

fn piped(stdin_text: &str, prog: &str) -> String {
    format!("echo '{stdin_text}' | python3 -c '{prog}'")
}

fn assert_deny_both(prog: &str, tag: &str) {
    let cmd = piped(HARM_STDIN, prog);
    let home = unique_home(tag);
    let (m, mr) = run_as(&cmd, &home, false);
    let (s, sr) = run_as(&cmd, &home, true);
    assert_eq!(m, "deny", "main must deny; cmd={cmd:?} reason={mr}");
    assert_eq!(s, "deny", "subagent must deny; cmd={cmd:?} reason={sr}");
}

#[test]
fn spaced_exec_input_with_harmful_stdin_is_deny() {
    assert_deny_both(SPACED_EXEC, "a");
}

#[test]
fn spaced_compile_input_with_harmful_stdin_is_deny() {
    assert_deny_both(SPACED_COMPILE, "b");
}

#[test]
#[ignore = "RED: 2026-11-15 python -c globals-alias exec(input()) with harmful stdin is allow — remove when fixed"]
fn globals_alias_exec_input_with_harmful_stdin_is_deny() {
    assert_deny_both(GLOBALS_ALIAS, "c");
}

#[test]
fn observation_benign_stdin_with_space_sensitive_spellings_is_allow() {
    let home = unique_home("obs");
    for prog in [SPACED_EXEC, SPACED_COMPILE, GLOBALS_ALIAS] {
        let cmd = piped("print(1)", prog);
        let (m, mr) = run_as(&cmd, &home, false);
        let (s, sr) = run_as(&cmd, &home, true);
        assert_eq!(m, "allow", "observation (main); cmd={cmd:?} reason={mr}");
        assert_eq!(s, "allow", "observation (sub); cmd={cmd:?} reason={sr}");
    }
}

#[test]
fn control_exec_input_with_harmful_stdin_is_deny() {
    assert_deny_both("exec(input())", "ctl");
}
