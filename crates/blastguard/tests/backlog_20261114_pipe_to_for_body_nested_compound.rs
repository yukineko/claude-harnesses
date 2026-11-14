#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (system-harm fail-open, observed 2026-11-14 on base b0b4ac6e3 and on
//! HEAD 9f709ecd9 / blastguard 0.2.573): a program that makes a system file
//! world-writable (`P` = `echo 'chmod 777 /etc/hosts'`), piped into a
//! top-level (unparenthesised) `for` loop whose body contains a closed nested
//! compound (`if..fi`, `while..done`, `{ ..; }`) BEFORE the stdin reader
//! (`bash`), is Allow in main AND subagent mode. Without the nested compound,
//! or with the whole loop inside `( .. )`, the same pipe is Deny.
//! Observed (main / subagent):
//!   P | for x in a; do bash; done                                    deny  / deny   (control)
//!   P | ( for x in a; do if true; then echo; fi; bash; done )        deny  / deny   (control)
//!   P | for x in a; do if true; then echo; fi; bash; done            allow / allow  <- fail-open
//!   P | for x in a; do while false; do :; done; bash; done           allow / allow  <- fail-open
//!   P | for x in a b; do { :; }; if true; then :; fi; bash; done     allow / allow  <- fail-open
//!   echo 'ls' | for x in a; do if true; then echo; fi; bash; done    allow / allow  (benign control)
//! Asserted (RED, `#[ignore]`d): every fail-open shape is Deny in both modes.
//! Controls (not ignored): the two Deny shapes stay Deny in both modes; the
//! benign `echo 'ls'` feeder into the nested-compound for body stays Allow in
//! both modes (guards against a fix that denies every such loop).
//!
//! DOES NOT PROVE: other harmful programs; `select`/`case` bodies; deeper
//! nesting levels; nested compounds AFTER the reader; readers other than
//! `bash`; heredoc/herestring feeders; that `while`/`until` outer loops with
//! the same body are handled (observed Deny at HEAD, not asserted here).
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
        "bg-forbody-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

/// Run the hook. Returns (decision, reason).
fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "pipe-for-body-nested-compound-20261114",
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

const HARM: &str = "echo 'chmod 777 /etc/hosts' | ";

const FAIL_OPEN: &[&str] = &[
    "for x in a; do if true; then echo; fi; bash; done",
    "for x in a; do while false; do :; done; bash; done",
    "for x in a b; do { :; }; if true; then :; fi; bash; done",
];

const CONTROLS: &[&str] = &[
    "for x in a; do bash; done",
    "( for x in a; do if true; then echo; fi; bash; done )",
];

/// Every (shape, mode) whose decision is not `want`.
fn mismatched(feeder: &str, shapes: &[&str], want: &str, tag: &str) -> Vec<String> {
    let home = unique_home(tag);
    let mut bad = Vec::new();
    for sh in shapes {
        let cmd = format!("{feeder}{sh}");
        for subagent in [false, true] {
            let (d, r) = run_as(&cmd, &home, subagent);
            if d != want {
                bad.push(format!("subagent={subagent} {cmd:?} => {d} ({r})"));
            }
        }
    }
    bad
}

#[test]
#[ignore = "RED: 2026-11-14 harmful program piped into a top-level for loop whose body has a closed nested compound before the reader is Allow — remove the ignore when fixed"]
fn harmful_program_piped_to_for_body_with_nested_compound_is_deny() {
    let bad = mismatched(HARM, FAIL_OPEN, "deny", "red");
    assert!(
        bad.is_empty(),
        "expected Deny for every shape:\n{}",
        bad.join("\n")
    );
}

#[test]
fn control_harmful_program_piped_to_handled_for_shapes_is_deny() {
    let bad = mismatched(HARM, CONTROLS, "deny", "ctl");
    assert!(
        bad.is_empty(),
        "control shapes must stay Deny:\n{}",
        bad.join("\n")
    );
}

#[test]
fn control_benign_program_piped_to_for_body_with_nested_compound_is_allow() {
    let bad = mismatched(
        "echo 'ls' | ",
        &["for x in a; do if true; then echo; fi; bash; done"],
        "allow",
        "benign",
    );
    assert!(
        bad.is_empty(),
        "benign feeder must stay Allow:\n{}",
        bad.join("\n")
    );
}
