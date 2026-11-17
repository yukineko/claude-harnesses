#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (false positive; user ruling 2026-11-18): zsh `=word` (equals)
//! expansion is SHELL-SCOPED.
//!
//! - Bash-tool top level (zsh) and `zsh -c '...'`: `=rm` expands to the PATH
//!   binary -> a harmful payload must be Deny (main and subagent).
//! - `bash -c '...'`: bash does NOT expand `=rm`; it is a command literally
//!   named `=rm` (not found). It must NOT be judged as rm: expected Allow (an
//!   unknown program). `env bash -c` / `sudo bash -c` follow bash.
//! - `sh -c '...'`: the inner shell is undeterminable (on macOS /bin/sh may be
//!   zsh) -> treated as expanding (strict): Deny.
//! - Nested `bash -c "zsh -c '=rm -rf ~'"`: inner zsh expands -> Deny.
//! - `bash -c 'rm -rf ~'` (no `=`) stays Deny (control).
//!
//! Fix f455e6b44 strips one leading `=` from the command word everywhere,
//! including inside `bash -c`, which is the false positive pinned here.
//!
//! DOES NOT PROVE: how bash itself treats `=rm` on this host (not executed
//! here; taken from the ruling); verdicts for other bash-only/zsh-only
//! expansions; `=word` in argument position; shells other than
//! bash/zsh/sh (dash, ksh, fish ...); `sudo` is asserted only "not Deny"
//! because `sudo` may carry its own Ask independent of this ruling.
//!
//! Written by an independent test author, not the implementer. The harness
//! mirrors tests/backlog_20261118_zsh_equals_cmd_fail_open.rs.

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
        "bg-zsh-equals-scope-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "zsh-equals-shell-scope-20261118",
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

/// Every (command, subagent) pair whose decision fails `ok`.
fn mismatches(cmds: &[&str], tag: &str, want: &str, ok: fn(&str) -> bool) -> Vec<String> {
    let home = unique_home(tag);
    let mut bad = Vec::new();
    for cmd in cmds {
        for subagent in [false, true] {
            let (d, r) = run_as(cmd, &home, subagent);
            if !ok(&d) {
                bad.push(format!(
                    "subagent={subagent} {cmd:?} => {d} (want {want}) ({r})"
                ));
            }
        }
    }
    bad
}

fn assert_none(bad: Vec<String>, what: &str) {
    assert!(bad.is_empty(), "{what}:\n{}", bad.join("\n"));
}

// ---- expanding shells: Deny (controls for the fix) ----

#[test]
fn top_level_equals_rm_is_deny() {
    let bad = mismatches(&["=rm -rf ~"], "top", "deny", |d| d == "deny");
    assert_none(bad, "Bash-tool top level (zsh) expands =rm: must be Deny");
}

#[test]
fn zsh_c_equals_rm_is_deny() {
    let bad = mismatches(&["zsh -c '=rm -rf ~'"], "zsh-c", "deny", |d| d == "deny");
    assert_none(bad, "zsh -c expands =rm: must be Deny");
}

#[test]
fn env_zsh_c_equals_rm_is_deny() {
    let bad = mismatches(&["env zsh -c '=rm -rf ~'"], "env-zsh", "deny", |d| {
        d == "deny"
    });
    assert_none(bad, "env zsh -c expands =rm: must be Deny");
}

#[test]
fn sh_c_equals_rm_is_deny_strict() {
    let bad = mismatches(&["sh -c '=rm -rf ~'"], "sh-c", "deny", |d| d == "deny");
    assert_none(
        bad,
        "sh -c: inner shell undeterminable, treated as expanding: must be Deny",
    );
}

#[test]
fn bash_c_nested_zsh_c_equals_rm_is_deny() {
    let bad = mismatches(&["bash -c \"zsh -c '=rm -rf ~'\""], "nested", "deny", |d| {
        d == "deny"
    });
    assert_none(bad, "inner zsh -c expands =rm: must be Deny");
}

#[test]
fn control_bash_c_plain_rm_is_deny() {
    let bad = mismatches(&["bash -c 'rm -rf ~'"], "ctl-bash-rm", "deny", |d| {
        d == "deny"
    });
    assert_none(bad, "control bash -c 'rm -rf ~' must stay Deny");
}

// ---- bash does not expand =word: not rm ----

#[test]
#[ignore = "RED: observed Deny (main+subagent) at f455e6b44: bash -c does not expand =rm"]
fn bash_c_equals_rm_is_allow() {
    let bad = mismatches(&["bash -c '=rm -rf ~'"], "bash-c", "allow", |d| {
        d == "allow"
    });
    assert_none(
        bad,
        "bash -c does not expand =rm (unknown program `=rm`): must be Allow",
    );
}

#[test]
#[ignore = "RED: observed Deny (main+subagent) at f455e6b44: env bash -c follows bash"]
fn env_bash_c_equals_rm_is_allow() {
    let bad = mismatches(&["env bash -c '=rm -rf ~'"], "env-bash", "allow", |d| {
        d == "allow"
    });
    assert_none(
        bad,
        "env bash -c follows bash (no =word expansion): must be Allow",
    );
}

#[test]
#[ignore = "RED: observed Deny (main+subagent) at f455e6b44: sudo bash -c follows bash"]
fn sudo_bash_c_equals_rm_is_not_deny() {
    let bad = mismatches(
        &["sudo bash -c '=rm -rf ~'"],
        "sudo-bash",
        "not deny",
        |d| d != "deny",
    );
    assert_none(
        bad,
        "sudo bash -c follows bash (no =word expansion): must not be Deny",
    );
}
