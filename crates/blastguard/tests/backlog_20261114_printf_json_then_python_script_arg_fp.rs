#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (false positive, observed 2026-11-14 by another agent):
//! `printf '%s\n' '[{"cmd":"echo hi"}]' > cases5.json && python3 probe.py cases5.json`
//! (probe.py exists in the worktree and only reads its argument) was denied:
//! "`printf` writes `cases5.json`, and another part of this command may run
//! that file with Python, so the written text is judged as the Python program it
//! is; its arguments (...) need printf's option or format-reuse rules, which are
//! not modelled...".
//!
//! Two readings: (1) real FP: cases5.json is an argument of probe.py, a data
//! file, not the program python runs; (2) intended: any written file also named
//! on a python command line is judged as a program. Tell apart: python's program
//! is its first non-option operand (probe.py); a later operand is argv. Control:
//! `python3 probe.py cases5.json` alone is allow.
//!
//! Asserted: subagent == "allow".
//!
//! DOES NOT PROVE: `python3 cases5.json` (running the written file; must stay
//! refused/judged), main mode.
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
        "bg-side1114-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "side-findings-20261114",
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

fn seed(home: &Home) {
    std::fs::write(
        home.0.join("wt").join("probe.py"),
        "import sys\nprint(open(sys.argv[1]).read())\n",
    )
    .unwrap();
}

#[test]
#[ignore = "RED: 2026-11-14 printf-written json passed as python script arg denied — remove the ignore when fixed"]
fn printf_json_then_python_script_with_json_arg_is_allow() {
    let home = unique_home("red");
    seed(&home);
    let cmd =
        "printf '%s\\n' '[{\"cmd\":\"echo hi\"}]' > cases5.json && python3 probe.py cases5.json";
    let (d, r) = run_as(cmd, &home, true);
    assert_eq!(d, "allow", "cmd={cmd:?} reason={r}");
}

#[test]
fn control_python_probe_alone_is_allow() {
    let home = unique_home("ctl");
    seed(&home);
    std::fs::write(home.0.join("wt").join("cases5.json"), "[]").unwrap();
    let (d, r) = run_as("python3 probe.py cases5.json", &home, true);
    assert_eq!(d, "allow", "control reason={r}");
}
