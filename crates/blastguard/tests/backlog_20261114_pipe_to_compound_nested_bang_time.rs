#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (system-harm fail-open, observed 2026-11-14 on rev 16b45552f, the
//! worktree of task b3569da5, blastguard 0.2.569): a program that makes a
//! system file world-writable (`P` = `echo 'chmod 777 /etc/hosts'`), piped into
//! an `if`/`while` body whose nested compound is prefixed with `!` or `time`,
//! is Allow in main AND subagent mode. The nesting count in
//! `detect/payload/compound_readers.rs` does not count an opener that follows
//! `!`/`time`, so the inner `fi`/`done` closes the outer compound early and the
//! trailing `bash` (which reads the pipe) is never judged.
//! Observed (main / subagent):
//!   P | if true; then if true; then echo; fi; bash; fi        deny  / deny   (control)
//!   P | if true; then ! if true; then echo; fi; bash; fi      allow / allow  <- fail-open
//!   P | if true; then time if true; then echo; fi; bash; fi   allow / allow  <- fail-open
//!   P | while true; do ! while false; do :; done; bash; done  allow / allow  <- fail-open
//!   P | if true; then ! { if true; then echo; fi; }; bash; fi allow / allow  <- fail-open
//!   echo 'ls' | if true; then ! if true; then echo; fi; bash; fi   allow / allow  (harmless control)
//! Asserted (RED, NOT ignored): every fail-open shape is Deny in both modes.
//! Controls (not ignored): the un-prefixed nested shape stays Deny, and the
//! harmless quoted program through the `!`-prefixed nested shape stays Allow
//! (the fix must not introduce a new false positive on that shape).
//!
//! DOES NOT PROVE: other prefix words (`time -p`, `! !`, `command`, `exec`),
//! `until`/`case`/`select`/`for` nested under `!`/`time`, deeper nesting,
//! other harmful programs, heredoc feeders, or that the harmless control stays
//! Allow for any other harmless program. The harmless control only shows no FP
//! for `ls`; it does not prove the reason the shape is Allow.
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
        "bg-nested-bang-time-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

/// Run the hook. Returns (decision, reason).
fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "pipe-compound-nested-bang-time-20261114",
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
    "if true; then ! if true; then echo; fi; bash; fi",
    "if true; then time if true; then echo; fi; bash; fi",
    "while true; do ! while false; do :; done; bash; done",
    "if true; then ! { if true; then echo; fi; }; bash; fi",
];

const DENY_CONTROLS: &[&str] = &["if true; then if true; then echo; fi; bash; fi"];

const HARMLESS_ALLOW_CONTROL: &str = "echo 'ls' | if true; then ! if true; then echo; fi; bash; fi";

fn undenied(shapes: &[&str], tag: &str) -> Vec<String> {
    let home = unique_home(tag);
    let mut bad = Vec::new();
    for sh in shapes {
        let cmd = format!("{HARM}{sh}");
        for subagent in [false, true] {
            let (d, r) = run_as(&cmd, &home, subagent);
            if d != "deny" {
                bad.push(format!("subagent={subagent} {cmd:?} => {d} ({r})"));
            }
        }
    }
    bad
}

#[test]
fn harmful_program_piped_to_nested_bang_or_time_compound_is_deny() {
    let bad = undenied(FAIL_OPEN, "red");
    assert!(
        bad.is_empty(),
        "expected Deny for every shape:\n{}",
        bad.join("\n")
    );
}

#[test]
fn control_harmful_program_piped_to_unprefixed_nested_compound_is_deny() {
    let bad = undenied(DENY_CONTROLS, "ctl");
    assert!(
        bad.is_empty(),
        "control shapes must stay Deny:\n{}",
        bad.join("\n")
    );
}

#[test]
fn control_harmless_program_piped_to_nested_bang_compound_is_allow() {
    let home = unique_home("fp");
    let mut bad = Vec::new();
    for subagent in [false, true] {
        let (d, r) = run_as(HARMLESS_ALLOW_CONTROL, &home, subagent);
        if d != "allow" {
            bad.push(format!(
                "subagent={subagent} {HARMLESS_ALLOW_CONTROL:?} => {d} ({r})"
            ));
        }
    }
    assert!(
        bad.is_empty(),
        "harmless control must stay Allow (no new FP):\n{}",
        bad.join("\n")
    );
}
