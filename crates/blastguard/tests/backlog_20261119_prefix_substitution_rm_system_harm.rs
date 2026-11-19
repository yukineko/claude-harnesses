#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (system-harm fail-open, observed 2026-11-19, main 5a27f8ecd, blastguard
//! 0.2.738, macOS, hermetic HOME): `L=/etc/hosts; y=$(L=/tmp/x; true) rm $L` is
//! Allow (empty stdout). In bash a prefix assignment `y=$(...)` runs the
//! substitution in a subshell, so `L` stays `/etc/hosts` and `rm $L` deletes
//! /etc/hosts. Control `L=/etc/hosts; rm $L` is deny.
//!
//! Two readings: (a) real fail-open: the substitution body's `L=/tmp/x` is
//! wrongly treated as rebinding the outer `L`; (b) intended. Distinguishing
//! check: bash semantics -- command substitution runs in a subshell, so its
//! assignments never reach the enclosing shell. System harm must be Deny
//! through any wrapper (user ruling).
//!
//! Asserted: verdict == "deny" in main and subagent contexts (ignored RED);
//! control deny un-ignored.
//!
//! Observed NOT Allow, so deliberately excluded: `y=$(L=/tmp/x) rm $L` (deny)
//! and the backtick spelling (main: ask, subagent: deny).
//!
//! Variants `y=$(L=/tmp/x) <rm> $L` (deny) and the backtick form (main: ask, not
//! Allow) were observed NOT to be Allow and are deliberately excluded.
//!
//! DOES NOT PROVE: backtick / process substitution, exact reason text, which
//! internal pass is responsible.
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
        "bg-prefixsubrm-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "prefix-sub-rm-20261119",
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

fn assert_deny(cmd: &str, tag: &str, subagent: bool) {
    let home = unique_home(tag);
    let (d, r) = run_as(cmd, &home, subagent);
    assert_eq!(
        d, "deny",
        "system harm must be deny; subagent={subagent} cmd={cmd:?} reason={r}"
    );
}

const PRIMARY: &str = "L=/etc/hosts; y=$(L=/tmp/x; true) rm $L";
const DASH_F: &str = "L=/etc/hosts; y=$(L=/tmp/x; true) rm -f $L";

#[test]
fn red_primary_main_is_deny() {
    assert_deny(PRIMARY, "p0", false);
}

#[test]
fn red_primary_subagent_is_deny() {
    assert_deny(PRIMARY, "p1", true);
}

#[test]
fn red_dash_f_main_is_deny() {
    assert_deny(DASH_F, "f0", false);
}

#[test]
fn red_dash_f_subagent_is_deny() {
    assert_deny(DASH_F, "f1", true);
}

#[test]
fn control_plain_rm_main_is_deny() {
    assert_deny("L=/etc/hosts; rm $L", "c0", false);
}

#[test]
fn control_plain_rm_subagent_is_deny() {
    assert_deny("L=/etc/hosts; rm $L", "c1", true);
}
