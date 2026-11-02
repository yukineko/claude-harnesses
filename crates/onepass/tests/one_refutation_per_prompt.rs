// Integration test file: unwrap/expect/panic are allowed here (the workspace
// lint deny targets production code).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Binary-level behaviour of `onepass gate` (PreToolUse on Agent/Task) and
//! `onepass reset` (UserPromptSubmit).

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

const RULE: &str = "1度の反証とテスト、証明できなければ破棄、テストがREDなら起票";

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

fn prompt_payload(session: &str) -> String {
    serde_json::json!({
        "session_id": session,
        "hook_event_name": "UserPromptSubmit",
        "prompt": "next"
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
fn first_matching_spawn_is_allowed() {
    let dir = tempfile::tempdir().unwrap();
    let o = run(
        dir.path(),
        "gate",
        &spawn_payload("s1", "Refute finding X", "general-purpose"),
    );
    assert_allowed(&o, "first refutation spawn");
}

#[test]
fn second_matching_spawn_in_same_prompt_is_denied_quoting_the_rule() {
    let dir = tempfile::tempdir().unwrap();
    let first = run(
        dir.path(),
        "gate",
        &spawn_payload("s1", "Verify the fix", "general-purpose"),
    );
    assert_allowed(&first, "first verification spawn");
    let second = run(
        dir.path(),
        "gate",
        &spawn_payload("s1", "audit round two", "general-purpose"),
    );
    assert_denied(&second, "second matching spawn");
    assert!(
        second.stdout.contains(RULE),
        "deny reason must quote the rule verbatim: {:?}",
        second.stdout
    );
    assert!(
        second.stdout.contains("何段階も深くほらない"),
        "deny reason must quote the rule verbatim: {:?}",
        second.stdout
    );
}

#[test]
fn match_is_case_insensitive_and_covers_subagent_type_and_japanese() {
    for (desc, ty) in [
        ("plain task", "my-VERIFIER"),
        ("ADVERSARIAL pass", "general-purpose"),
        ("反証する", "general-purpose"),
        ("検証", "general-purpose"),
        ("監査ラウンド", "general-purpose"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let first = run(dir.path(), "gate", &spawn_payload("s1", desc, ty));
        assert_allowed(&first, desc);
        let second = run(dir.path(), "gate", &spawn_payload("s1", desc, ty));
        assert_denied(&second, desc);
    }
}

#[test]
fn non_matching_spawns_are_never_touched() {
    let dir = tempfile::tempdir().unwrap();
    // Spend this prompt's one refutation first.
    let first = run(
        dir.path(),
        "gate",
        &spawn_payload("s1", "refute it", "general-purpose"),
    );
    assert_allowed(&first, "first refutation");
    for _ in 0..3 {
        let o = run(
            dir.path(),
            "gate",
            &spawn_payload("s1", "Search for config files", "Explore"),
        );
        assert_allowed(&o, "non-matching spawn");
        assert!(
            o.stdout.trim().is_empty(),
            "non-matching spawn must produce no output: {:?}",
            o.stdout
        );
    }
}

#[test]
fn user_prompt_submit_resets_the_counter() {
    let dir = tempfile::tempdir().unwrap();
    let p = spawn_payload("s1", "verify", "general-purpose");
    assert_allowed(&run(dir.path(), "gate", &p), "prompt 1 first");
    assert_denied(&run(dir.path(), "gate", &p), "prompt 1 second");

    let r = run(dir.path(), "reset", &prompt_payload("s1"));
    assert_eq!(r.code, 0, "reset must not block the user's prompt");

    assert_allowed(&run(dir.path(), "gate", &p), "prompt 2 first");
    assert_denied(&run(dir.path(), "gate", &p), "prompt 2 second");
}

#[test]
fn sessions_are_counted_separately() {
    let dir = tempfile::tempdir().unwrap();
    assert_allowed(
        &run(dir.path(), "gate", &spawn_payload("a", "verify", "x")),
        "session a",
    );
    assert_allowed(
        &run(dir.path(), "gate", &spawn_payload("b", "verify", "x")),
        "session b",
    );
    // Resetting b does not reset a.
    run(dir.path(), "reset", &prompt_payload("b"));
    assert_denied(
        &run(dir.path(), "gate", &spawn_payload("a", "verify", "x")),
        "session a second",
    );
}

#[test]
fn corrupt_state_denies_a_matching_spawn() {
    let dir = tempfile::tempdir().unwrap();
    // The state directory path is a regular file: state can be neither read
    // nor written.
    let not_a_dir = dir.path().join("state");
    std::fs::write(&not_a_dir, b"garbage").unwrap();
    let o = run(
        &not_a_dir,
        "gate",
        &spawn_payload("s1", "verify", "general-purpose"),
    );
    assert_denied(&o, "unusable state");
}

#[test]
fn corrupt_state_does_not_touch_a_non_matching_spawn() {
    let dir = tempfile::tempdir().unwrap();
    let not_a_dir = dir.path().join("state");
    std::fs::write(&not_a_dir, b"garbage").unwrap();
    let o = run(
        &not_a_dir,
        "gate",
        &spawn_payload("s1", "Search code", "Explore"),
    );
    assert_allowed(&o, "non-matching spawn with unusable state");
}

#[test]
fn unparseable_payload_denies() {
    let dir = tempfile::tempdir().unwrap();
    let o = run(dir.path(), "gate", "{not json, refute");
    assert_denied(&o, "unparseable payload");
}

#[test]
fn matching_spawn_without_session_id_denies() {
    let dir = tempfile::tempdir().unwrap();
    let o = run(
        dir.path(),
        "gate",
        &spawn_payload("", "verify", "general-purpose"),
    );
    assert_denied(&o, "no session id");
}
