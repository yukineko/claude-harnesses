// Integration test file: unwrap/expect/panic are allowed here (the workspace
// lint deny targets production code).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! condukt-spawned roles (subagent_type starting with `condukt:`) are exempt
//! from the one-refutation slot: never denied, and never take the slot.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

struct Out {
    code: i32,
    stdout: String,
}

fn run(state_dir: &Path, sub: &str, stdin: &str) -> Out {
    let mut child = Command::new(env!("CARGO_BIN_EXE_onepass"))
        .arg(sub)
        .env("ONEPASS_STATE_DIR", state_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn onepass");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(stdin.as_bytes())
        .expect("write stdin");
    let out = child.wait_with_output().expect("wait");
    Out {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
    }
}

fn spawn_payload(session: &str, description: &str, subagent_type: &str) -> String {
    serde_json::json!({
        "session_id": session,
        "hook_event_name": "PreToolUse",
        "tool_name": "Agent",
        "tool_input": {
            "description": description,
            "subagent_type": subagent_type,
            "prompt": "do the thing"
        }
    })
    .to_string()
}

/// The decision JSON's `permissionDecision`, if stdout carries one.
fn decision(o: &Out) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(o.stdout.trim()).ok()?;
    v["hookSpecificOutput"]["permissionDecision"]
        .as_str()
        .map(str::to_string)
}

fn assert_allowed(o: &Out, what: &str) {
    assert_eq!(o.code, 0, "{what}: exit code (stdout={:?})", o.stdout);
    assert_ne!(
        decision(o).as_deref(),
        Some("deny"),
        "{what}: must not be denied (stdout={:?})",
        o.stdout
    );
}

fn assert_denied(o: &Out, what: &str) {
    assert_eq!(o.code, 0, "{what}: exit code (stdout={:?})", o.stdout);
    assert_eq!(
        decision(o).as_deref(),
        Some("deny"),
        "{what}: must be denied (stdout={:?})",
        o.stdout
    );
}

#[test]
fn condukt_verifier_after_slot_taken_is_not_denied() {
    let dir = tempfile::tempdir().unwrap();
    let first = run(
        dir.path(),
        "gate",
        &spawn_payload("s1", "verify X", "general-purpose"),
    );
    assert_allowed(&first, "slot-taking spawn");
    let o = run(
        dir.path(),
        "gate",
        &spawn_payload("s1", "verify task t1", "condukt:condukt-verifier"),
    );
    assert_allowed(&o, "condukt verifier after slot taken");
}

#[test]
fn condukt_researcher_after_slot_taken_is_not_denied() {
    let dir = tempfile::tempdir().unwrap();
    let first = run(
        dir.path(),
        "gate",
        &spawn_payload("s1", "verify X", "general-purpose"),
    );
    assert_allowed(&first, "slot-taking spawn");
    let o = run(
        dir.path(),
        "gate",
        &spawn_payload("s1", "audit cache research", "condukt:condukt-researcher"),
    );
    assert_allowed(&o, "condukt researcher after slot taken");
}

#[test]
fn condukt_role_does_not_take_the_slot() {
    let dir = tempfile::tempdir().unwrap();
    let c = run(
        dir.path(),
        "gate",
        &spawn_payload("s1", "verify t1", "condukt:condukt-verifier"),
    );
    assert_allowed(&c, "condukt verifier on fresh session");
    let a = run(
        dir.path(),
        "gate",
        &spawn_payload("s1", "refute finding", "general-purpose"),
    );
    assert_allowed(&a, "first non-condukt spawn after condukt role");
    let b = run(
        dir.path(),
        "gate",
        &spawn_payload("s1", "refute finding 2", "general-purpose"),
    );
    assert_denied(&b, "second non-condukt spawn (control)");
}

#[test]
fn subagent_type_merely_containing_condukt_is_still_counted() {
    let dir = tempfile::tempdir().unwrap();
    let first = run(
        dir.path(),
        "gate",
        &spawn_payload("s1", "verify X", "general-purpose"),
    );
    assert_allowed(&first, "slot-taking spawn");
    let o = run(
        dir.path(),
        "gate",
        &spawn_payload("s1", "verify task t1", "my-condukt-verifier"),
    );
    assert_denied(&o, "my-condukt-verifier is not a condukt: role");
}
