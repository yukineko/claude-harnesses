#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Controls for backlog cffe54c4 (alias-shadowed echo/printf feeder).
//! (A) shapes whose decision must not change (Allow: no new false positive);
//! (B) alias-shadowed feeders that must be Ask or Deny.
//!
//! DOES NOT PROVE: real shell alias expansion (nothing is executed); aliases
//! spelled by indirection; reason text.

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
        "bg-aliasemit-ctl-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn decide(command: &str, home: &Home, subagent: bool) -> String {
    let mut p = serde_json::json!({
        "session_id": "aliasemit-controls-20261121",
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
fn a_plain_procsub_echo_is_allow() {
    expect_allow("bash <(echo hi)", "a-plain");
}

#[test]
fn a_unrelated_alias_then_procsub_is_allow() {
    expect_allow("alias ll='ls -l'; bash <(echo hi)", "a-ll");
}

#[test]
fn b_alias_echo_is_not_allow() {
    expect_not_allow(
        "alias echo='echo chmod 777 /etc/hosts #'; bash <(echo hi)",
        "b-echo",
    );
}

#[test]
fn b_alias_printf_pipe_is_not_allow() {
    expect_not_allow(
        "alias printf='echo chmod 777 /etc/hosts #'; printf hi | bash",
        "b-printf",
    );
}

#[test]
fn b_alias_nonliteral_name_is_not_allow() {
    expect_not_allow("alias $n='x'; bash <(echo hi)", "b-nonlit");
}

#[test]
fn a_alias_inside_quotes_is_allow() {
    expect_allow(r#"echo "alias echo=x"; bash <(echo hi)"#, "a-quoted");
}

#[test]
fn a_alias_as_grep_argument_is_allow() {
    expect_allow(r#"grep -c alias "$f"; bash <(echo hi)"#, "a-grep");
}

#[test]
fn a_alias_as_echo_argument_with_expansion_is_allow() {
    expect_allow("echo alias $HOME; bash <(echo hi)", "a-echo-home");
}

#[test]
fn b_alias_backslash_spelled_is_not_allow() {
    expect_not_allow(
        r"ali\as echo='echo chmod 777 /etc/hosts #'; bash <(echo hi)",
        "b-backslash",
    );
}

#[test]
fn b_alias_empty_quotes_spelled_is_not_allow() {
    expect_not_allow(
        r#"a""lias echo='echo chmod 777 /etc/hosts #'; bash <(echo hi)"#,
        "b-emptyquote",
    );
}
