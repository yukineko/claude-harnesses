#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (fail-open, system harm; observed 2026-11-18 in session 1e000416):
//! Claude Code's Bash tool runs commands with /bin/zsh 5.9
//! (`zsh -c 'source <snapshot> && ... eval '<cmd>''`), where option `equals`
//! is ON. Observed in that shell: `echo =ls` printed `/bin/ls`; `=echo hi`
//! printed `hi`; `=true && echo command-position-equals-runs` printed the
//! marker. So a leading `=word` in command position runs the real binary
//! found on PATH.
//!
//! The deployed blastguard 0.2.690, fed a PreToolUse Bash payload on stdin,
//! returned EMPTY stdout, exit 0 (= allow) for `=rm -rf ~`,
//! `=chmod 777 /etc/hosts` and `=bash -c 'chmod 777 /etc/hosts'`, in both main
//! and subagent (payload with "agent_id"). The controls `rm -rf ~` and
//! `chmod 777 /etc/hosts` are Deny ("[system harm ...]").
//!
//! Standing rule: system-harm targets must be Deny through any wrapper.
//!
//! DOES NOT PROVE: which fix is right; the verdict for a harmless `=echo hi`
//! (deliberately not asserted); `=word` in argument position; other zsh-only
//! expansions.
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
        "bg-zsh-equals-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "zsh-equals-cmd-fail-open-20261118",
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

#[test]
#[ignore = "RED: 2026-11-18 zsh `=rm -rf ~` (equals expansion in command position) is Allow — remove the ignore when fixed"]
fn equals_rm_rf_home_is_deny() {
    let bad = mismatches(&["=rm -rf ~"], "red-rm", "deny", |d| d == "deny");
    assert!(
        bad.is_empty(),
        "system harm via =cmd must be Deny:\n{}",
        bad.join("\n")
    );
}

#[test]
#[ignore = "RED: 2026-11-18 zsh `=chmod 777 /etc/hosts` (equals expansion in command position) is Allow — remove the ignore when fixed"]
fn equals_chmod_etc_hosts_is_deny() {
    let bad = mismatches(&["=chmod 777 /etc/hosts"], "red-chmod", "deny", |d| {
        d == "deny"
    });
    assert!(
        bad.is_empty(),
        "system harm via =cmd must be Deny:\n{}",
        bad.join("\n")
    );
}

#[test]
#[ignore = "RED: 2026-11-18 zsh `=bash -c 'chmod 777 /etc/hosts'` is Allow — remove the ignore when fixed"]
fn equals_bash_c_chmod_is_not_allow() {
    let bad = mismatches(
        &["=bash -c 'chmod 777 /etc/hosts'"],
        "red-bash",
        "not allow",
        |d| d != "allow",
    );
    assert!(
        bad.is_empty(),
        "=bash -c <harm> must not be Allow:\n{}",
        bad.join("\n")
    );
}

#[test]
fn control_plain_system_harm_is_deny() {
    let bad = mismatches(
        &["rm -rf ~", "chmod 777 /etc/hosts"],
        "ctl-harm",
        "deny",
        |d| d == "deny",
    );
    assert!(
        bad.is_empty(),
        "control must stay Deny:\n{}",
        bad.join("\n")
    );
}

#[test]
fn control_echo_hi_is_allow() {
    let bad = mismatches(&["echo hi"], "ctl-echo", "allow", |d| d == "allow");
    assert!(
        bad.is_empty(),
        "control must stay Allow:\n{}",
        bad.join("\n")
    );
}

// Pins of the verdicts observed on 2026-11-18 (worktree build at origin/main
// c3dd9fd6d, main and subagent): the fix must not change them.

#[test]
fn control_pin_equals_shapes_outside_command_position_are_allow() {
    let bad = mismatches(
        &["echo =x", "a==b", "[[ $a == b ]]", "x=1 echo hi"],
        "ctl-pin",
        "allow",
        |d| d == "allow",
    );
    assert!(bad.is_empty(), "pinned Allow changed:\n{}", bad.join("\n"));
}

#[test]
fn control_pin_undetermined_equals_command_word_is_ask_main_deny_subagent() {
    // Observed already Ask (main) / Deny (subagent) before the fix:
    // "the command word `=$x` is an expansion whose value only exists at run time".
    let home = unique_home("ctl-undet");
    let mut bad = Vec::new();
    for (subagent, want) in [(false, "ask"), (true, "deny")] {
        let (d, r) = run_as("=$x foo", &home, subagent);
        if d != want {
            bad.push(format!(
                "subagent={subagent} \"=$x foo\" => {d} (want {want}) ({r})"
            ));
        }
    }
    assert!(
        bad.is_empty(),
        "pinned Undetermined changed:\n{}",
        bad.join("\n")
    );
}
