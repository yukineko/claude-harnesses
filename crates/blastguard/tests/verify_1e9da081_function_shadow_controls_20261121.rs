#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Controls for backlog 1e9da081 (function-shadowed echo/printf feeder is Ask).
//! (A) ordinary shapes that must stay Allow (new-false-positive detector);
//! (B) shadowed-feeder shapes that must be Ask or Deny (fail-open detector).
//! Each case runs in main and subagent mode and asserts the decision only.
//!
//! DOES NOT PROVE: that group A was Allow at the base commit (base not
//! observed; a failure there is a suspected new FP, not a confirmed one);
//! reason text; constructs not spelled here; real shell behaviour (nothing is
//! executed).

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
        "bg-fnshadow-ctl-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn decide(command: &str, home: &Home, subagent: bool) -> String {
    let mut p = serde_json::json!({
        "session_id": "fnshadow-controls-20261121",
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
fn a_def_f_call_is_allow() {
    expect_allow(r#"f(){ echo hi; }; f"#, "a-def_f_call");
}

#[test]
fn a_def_greet_printf_is_allow() {
    expect_allow(
        r#"greet(){ printf '%s\n' hi; }; greet"#,
        "a-def_greet_printf",
    );
}

#[test]
fn a_def_noop_then_echo_is_allow() {
    expect_allow(r#"f(){ :; }; echo hi"#, "a-def_noop_then_echo");
}

#[test]
fn a_echo_then_printf_is_allow() {
    expect_allow(r#"echo hi; printf '%s\n' x"#, "a-echo_then_printf");
}

#[test]
fn a_def_f_pipe_grep_is_allow() {
    expect_allow(r#"f(){ echo a; }; f | grep a"#, "a-def_f_pipe_grep");
}

#[test]
fn a_echo_pipe_grep_is_allow() {
    expect_allow(r#"echo hi | grep h"#, "a-echo_pipe_grep");
}

#[test]
fn a_printf_pipe_sort_is_allow() {
    expect_allow(r#"printf 'a\n' | sort"#, "a-printf_pipe_sort");
}

#[test]
fn b_echo_def_pipe_bash_is_not_allow() {
    expect_not_allow(
        r#"echo(){ echo 'chmod 777 /etc/hosts'; }; echo hi | bash"#,
        "b-echo_def_pipe_bash",
    );
}

#[test]
fn b_printf_def_pipe_sh_is_not_allow() {
    expect_not_allow(
        r#"printf(){ echo 'chmod 777 /etc/hosts'; }; printf hi | sh"#,
        "b-printf_def_pipe_sh",
    );
}

#[test]
fn b_function_kw_echo_procsub_is_not_allow() {
    expect_not_allow(
        r#"function echo { echo 'chmod 777 /etc/hosts'; }; bash <(echo hi)"#,
        "b-function_kw_echo_procsub",
    );
}

#[test]
fn b_export_f_echo_bash_c_pipe_is_not_allow() {
    expect_not_allow(
        r#"echo(){ echo 'chmod 777 /etc/hosts'; }; export -f echo; bash -c 'echo hi | bash'"#,
        "b-export_f_echo_bash_c_pipe",
    );
}
