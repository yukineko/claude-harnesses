#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (false positive, observed 2026-11-14 at main a67c5d23f): a `python3 -c`
//! program whose STRING LITERAL holds `>> ` followed by a backtick span,
//! `python3 -c 'print("x >> `y`")'`, is read as a shell append redirect to a
//! run-time path: Ask with the verbatim reason
//!
//!   '>> `y`' appends to a path that only exists at run time — blastguard
//!   cannot tell whether it is a protected gate/config file or a system directory
//!
//! Python never executes backticks (they are a syntax error in Python 3, not a
//! command substitution), so the text is data. Suspected path (UNVERIFIED, not
//! proven by this test): crates/blastguard/src/detect/interpreter.rs
//! `redirects_are_data` (~968) with runs_shell_strings / has_dynamic_code_execution
//! counting a bare backtick.
//!
//! Sibling of backlog_20261113_python_heredoc_body_append_backtick_fp.rs
//! (heredoc form); this is the `-c` argv form.
//!
//! Controls (GREEN at HEAD, not ignored): a `python3 -c` program that really
//! hands a `>> $v` string to a shell (os.system) is not Allow.
//!
//! DOES NOT PROVE: which stage misreads the text; anything about shell-reaching
//! python programs beyond the one control.

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
        "bg-pyc-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

/// Run the hook. Returns (decision, reason).
fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "py-c-append-20261114",
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

const C_WITH_BACKTICK: &str = "python3 -c 'print(\"x >> `y`\")'";
const C_SHELL_REACHING: &str = "python3 -c 'import os; os.system(\"echo x >> $v\")'";

// test-weakening-justified: new RED for an unfixed false positive (backlog
// pending, 2026-11-14); the ignore is removed in the fixing commit.
#[test]
#[ignore = "RED: <backlog-pending> 2026-11-14 python3 -c string literal with append-then-backtick text read as run-time append redirect; remove the ignore when fixed"]
fn python_c_string_literal_append_backtick_text_is_not_a_redirect() {
    let home = unique_home("red");
    for subagent in [false, true] {
        let (d, r) = run_as(C_WITH_BACKTICK, &home, subagent);
        assert!(
            !r.contains("appends to a path"),
            "subagent={subagent}: python -c string literal read as an append redirect: {d} | {r}"
        );
        assert!(d != "deny" && d != "ask", "subagent={subagent}: {d} | {r}");
    }
}

// Control: a python -c program that hands `>> $v` to a shell is not Allow.
#[test]
fn control_python_c_os_system_append_to_runtime_path_is_not_allowed() {
    let home = unique_home("ctl");
    for subagent in [false, true] {
        let (d, r) = run_as(C_SHELL_REACHING, &home, subagent);
        assert_ne!(d, "allow", "subagent={subagent}: {d} | {r}");
    }
}
