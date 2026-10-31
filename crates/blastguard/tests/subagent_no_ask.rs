// このファイルは丸ごと integration test なので unwrap/expect を許可する。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! **Inside a subagent blastguard must not ask.**
//!
//! A background subagent's Bash call got PreToolUse `ask`; nobody could answer
//! and it waited 10.6 h. `interactive::ask_available()` reads the env
//! (`CLAUDECODE=1` + `CLAUDE_CODE_ENTRYPOINT=cli`), and a subagent inherits it:
//! measured, the env is byte-identical between a main-thread and a subagent
//! call. The only discriminator is the payload: a subagent PreToolUse payload
//! carries a non-empty `agent_id` (and `agent_type`); the main thread's has
//! neither.
//!
//! User ruling: inside a subagent Ask becomes Deny, and the repeated-deny to
//! ask downgrade must also stay Deny. The main thread keeps its behaviour.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn scratch_home(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "bg-subask-{}-{}-{:?}",
        name,
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn payload(command: &str, agent_id: Option<&str>) -> String {
    let mut v = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": { "command": command },
    });
    if let Some(id) = agent_id {
        v["agent_id"] = serde_json::json!(id);
        v["agent_type"] = serde_json::json!("general-purpose");
    }
    v.to_string()
}

/// Run the real binary with the interactive env pinned (as a subagent inherits
/// it) and return the `permissionDecision` (`"allow"` when nothing printed).
fn verdict(home: &PathBuf, session: &str, command: &str, agent_id: Option<&str>) -> String {
    let bin = env!("CARGO_BIN_EXE_blastguard");
    let mut child = Command::new(bin)
        .env("HOME", home)
        .env("CLAUDE_CODE_SESSION_ID", session)
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env_remove("BLASTGUARD_ASK")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("binary spawns");
    if let Some(mut si) = child.stdin.take() {
        let _ = si.write_all(payload(command, agent_id).as_bytes());
    }
    let out = child.wait_with_output().expect("binary runs");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if stdout.trim().is_empty() {
        return "allow".to_string();
    }
    let doc: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout is not JSON ({e}): {stdout}"));
    doc.get("hookSpecificOutput")
        .and_then(|h| h.get("permissionDecision"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| panic!("no permissionDecision in {stdout}"))
}

const ASK_CMD: &str = r#"for f in a b; do git checkout -- "$f"; done"#;

/// CONTROL: main thread (no agent_id) with the interactive env still asks.
#[test]
fn main_thread_ask_is_unchanged() {
    let home = scratch_home("main-ask");
    assert_eq!(verdict(&home, "sessA", ASK_CMD, None), "ask");
}

/// CONTROL, anti-vacuity: the subagent payload is accepted and a plain deny
/// still denies (so the marker alone does not change a deny).
#[test]
fn subagent_plain_deny_is_deny() {
    let home = scratch_home("sub-deny");
    assert_eq!(
        verdict(
            &home,
            "sessA",
            "git reset --hard",
            Some("ab0687739fbba8a8b")
        ),
        "deny"
    );
}

#[test]
fn subagent_ask_becomes_deny() {
    let home = scratch_home("sub-ask");
    assert_eq!(
        verdict(&home, "sessA", ASK_CMD, Some("ab0687739fbba8a8b")),
        "deny",
        "inside a subagent nobody can answer an ask; it must be a deny"
    );
}

#[test]
fn subagent_repeated_deny_stays_deny() {
    let home = scratch_home("sub-repeat");
    let agent = Some("ab0687739fbba8a8b");
    assert_eq!(verdict(&home, "sessA", "git reset --hard", agent), "deny");
    assert_eq!(
        verdict(&home, "sessA", "git reset --hard", agent),
        "deny",
        "a repeat inside a subagent must not come back as ask"
    );
}

/// Like `verdict`, but sends `raw` verbatim as the stdin payload.
fn verdict_raw(home: &PathBuf, raw: &str) -> String {
    let bin = env!("CARGO_BIN_EXE_blastguard");
    let mut child = Command::new(bin)
        .env("HOME", home)
        .env("CLAUDE_CODE_SESSION_ID", "sessRaw")
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env_remove("BLASTGUARD_ASK")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("binary spawns");
    if let Some(mut si) = child.stdin.take() {
        let _ = si.write_all(raw.as_bytes());
    }
    let out = child.wait_with_output().expect("binary runs");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if stdout.trim().is_empty() {
        return "allow".to_string();
    }
    let doc: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout is not JSON ({e}): {stdout}"));
    doc.get("hookSpecificOutput")
        .and_then(|h| h.get("permissionDecision"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| panic!("no permissionDecision in {stdout}"))
}

/// CONTROL: an unreadable payload on the main thread asks (the interactive env).
#[test]
fn main_thread_unreadable_payload_asks() {
    let home = scratch_home("raw-main");
    assert_eq!(verdict_raw(&home, r#"{"tool_name":"Bash","tool_in"#), "ask");
}

/// An unreadable payload that still shows a raw agent_id key is a subagent:
/// the UNREADABLE_PAYLOAD ask must become a deny (fail-closed wiring in main).
#[test]
fn subagent_unreadable_payload_is_deny() {
    let home = scratch_home("raw-sub");
    assert_eq!(
        verdict_raw(&home, r#"{"agent_id":"ab06","tool_name":"Bash","tool_in"#),
        "deny"
    );
}

/// A non-string `agent_id` (null) in an otherwise valid payload is still a
/// subagent end to end: the ask must become a deny.
#[test]
fn subagent_with_null_agent_id_ask_becomes_deny() {
    let home = scratch_home("null-id");
    let mut v: serde_json::Value = serde_json::from_str(&payload(ASK_CMD, None)).unwrap();
    v["agent_id"] = serde_json::Value::Null;
    assert_eq!(verdict_raw(&home, &v.to_string()), "deny");
}
