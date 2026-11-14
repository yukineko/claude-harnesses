#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (false positive, observed 2026-11-14 at main b823ffa5 in a subagent, run
//! run-20261114-130020-46995): a version-bump `perl -pi -e` whose program is
//! SINGLE-QUOTED and uses perl's `$.` (current line number) is denied as
//! "`perl` is invoked with an inline-eval flag, and that program contains a
//! shell expansion (`if ($.==60) { s/"0\.2\.578"/"0.2.579"/ }`) — the text on
//! the command line is a template, not the program that will run". Inside
//! single quotes the shell expands nothing, so the program IS the literal text.
//!
//! Asserted: the single-quoted shape is Allow in subagent mode (it edits a
//! repo-relative file in cwd only).
//! Controls (GREEN at HEAD):
//!  - the same edit without `$.` is Allow;
//!  - a double-quoted perl program containing a real `$(...)` expansion is not Allow.
//!
//! DOES NOT PROVE: anything about main mode, or about perl programs that do
//! more than an in-place substitution.

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
        "bg-perldot-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    std::fs::write(dir.join("wt/m.json"), "{\"version\": \"0.2.578\"}\n").unwrap();
    Home(dir)
}

fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "perl-dollar-dot-fp-20261114",
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

// test-weakening-justified: new RED for an unfixed defect (side finding of run
// run-20261114-130020-46995, 2026-11-14); the ignore is removed in the fixing commit.
#[test]
#[ignore = "RED: 2026-11-14 perl's `$.` inside a SINGLE-quoted -e program is read as a shell expansion — remove the ignore when fixed"]
fn perl_single_quoted_program_with_dollar_dot_is_allowed() {
    let home = unique_home("red");
    let c = "perl -pi -e 'if ($.==1) { s/\"0\\.2\\.578\"/\"0.2.579\"/ }' m.json";
    let (d, r) = run_as(c, &home, true);
    assert_eq!(d, "allow", "{c:?} -> {d} | {r}");
}

// Control 1: same edit without `$.` is Allow.
#[test]
fn control_perl_single_quoted_without_dollar_dot_is_allowed() {
    let home = unique_home("ctl1");
    let c = "perl -pi -e 's/\"0\\.2\\.578\"/\"0.2.579\"/' m.json";
    let (d, r) = run_as(c, &home, true);
    assert_eq!(d, "allow", "{c:?} -> {d} | {r}");
}

// Control 2: a double-quoted program with a real command substitution is not Allow.
#[test]
fn control_perl_double_quoted_cmdsub_is_not_allowed() {
    let home = unique_home("ctl2");
    let c = "perl -pi -e \"s/x/$(cat /etc/passwd)/\" m.json";
    let (d, r) = run_as(c, &home, true);
    assert_ne!(d, "allow", "{c:?} -> {d} | {r}");
}
