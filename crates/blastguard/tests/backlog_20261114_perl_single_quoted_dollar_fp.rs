#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (2026-11-14, flow-8cca7ef5 orchestrator — not an implementer):
//! a read-only `perl -ne` program inside SINGLE quotes that uses a perl
//! special variable (`$.`) is refused as if it held a shell expansion.
//!
//! Observed live on the deployed hook (subagent mode, 2026-11-14):
//! `perl -ne 'print if $.==9' /dev/null` was denied with
//! "`perl` is invoked with an inline-eval flag, and that program contains a
//! shell expansion (`print if $.==9`) — the text on the command line is a
//! template, not the program that will run". Single quotes make the shell
//! pass `$.` through literally: the text IS the program, so this is a false
//! positive.
//!
//! Asserted (RED): Allow in main and subagent mode.
//! Control (not ignored): the double-quoted twin `perl -ne "print $HOME"`
//! really is a template (the shell expands `$HOME`) and is not Allow.
//!
//! DOES NOT PROVE: perl -i / -pe write programs, other perl variables
//! (`$_`, `@ARGV`), ruby/python twins, or mixed-quote words.

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
        "bg-perlsq-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

/// Run the hook; returns the permissionDecision ("allow" on empty stdout).
fn decide(command: &str, home: &Home, subagent: bool) -> String {
    let mut p = serde_json::json!({
        "session_id": "perl-single-quoted-dollar-20261114",
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
    println!("[{command:?}] subagent={subagent} => {:?}", stdout.trim());
    if stdout.trim().is_empty() {
        return "allow".into();
    }
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    v["hookSpecificOutput"]["permissionDecision"]
        .as_str()
        .unwrap_or("?")
        .to_string()
}

#[test]
#[ignore = "RED: single-quoted perl program using $. is refused as a shell expansion (backlog pending)"]
fn single_quoted_perl_special_variable_is_allowed() {
    let home = unique_home("red");
    let cmd = "perl -ne 'print if $.==9' /dev/null";
    let main = decide(cmd, &home, false);
    let sub = decide(cmd, &home, true);
    assert!(
        main == "allow" && sub == "allow",
        "main={main} subagent={sub}"
    );
}

#[test]
fn control_double_quoted_expansion_is_not_allowed() {
    let home = unique_home("ctl");
    let cmd = "perl -ne \"print $HOME\" /dev/null";
    assert_ne!(decide(cmd, &home, false), "allow");
    assert_ne!(decide(cmd, &home, true), "allow");
}
