#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Follow-up to t2 (function-shadowed echo/printf feeder): a literal feeder
//! whose text merely contains the substring "functions" (no function
//! definition anywhere) must stay Allow; a real `echo(){...}` definition must
//! still be Ask|Deny. Each case runs in main and subagent mode and asserts the
//! decision only.
//!
//! PROVES (when run): the decision for the four spelled commands in both modes.
//! DOES NOT PROVE: base behaviour (measure separately on the base commit);
//! reason text; other spellings of the substring; real shell behaviour
//! (nothing is executed).

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
        "bg-fnsubstr-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn decide(command: &str, home: &Home, subagent: bool) -> String {
    let mut p = serde_json::json!({
        "session_id": "fnsubstr-20261121",
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
        return "allow".into();
    }
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    v["hookSpecificOutput"]["permissionDecision"]
        .as_str()
        .unwrap_or("?")
        .to_string()
}

fn expect_allow(cmd: &str, tag: &str) {
    let home = unique_home(tag);
    let mut bad = Vec::new();
    for subagent in [false, true] {
        let d = decide(cmd, &home, subagent);
        if d != "allow" {
            bad.push(format!("subagent={subagent} => {d}"));
        }
    }
    assert!(
        bad.is_empty(),
        "expected Allow for {cmd:?}:\n{}",
        bad.join("\n")
    );
}

fn expect_not_allow(cmd: &str, tag: &str) {
    let home = unique_home(tag);
    let mut bad = Vec::new();
    for subagent in [false, true] {
        let d = decide(cmd, &home, subagent);
        if d != "ask" && d != "deny" {
            bad.push(format!("subagent={subagent} => {d}"));
        }
    }
    assert!(
        bad.is_empty(),
        "expected Ask|Deny for {cmd:?}:\n{}",
        bad.join("\n")
    );
}

#[test]
fn a_echo_functions_dir_pipe_bash_is_allow() {
    expect_allow(r#"echo 'ls functions/' | bash"#, "a-echo_pipe_bash");
}

#[test]
fn a_printf_functions_py_pipe_bash_is_allow() {
    expect_allow(
        r#"printf 'git log -- functions.py\n' | bash"#,
        "a-printf_pipe_bash",
    );
}

#[test]
fn a_procsub_echo_functions_dir_is_allow() {
    expect_allow(r#"bash <(echo 'ls functions/')"#, "a-procsub");
}

#[test]
fn b_real_echo_def_with_functions_text_is_not_allow() {
    expect_not_allow(
        r#"echo(){ echo 'chmod 777 /etc/hosts'; }; echo 'ls functions/' | bash"#,
        "b-real_def",
    );
}
