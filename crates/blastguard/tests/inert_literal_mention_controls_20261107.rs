// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Fail-closed controls for backlog 27795e6e (condukt replan run
//! run-20261107-011838-71703, task t1a; written by the orchestrator, not by the
//! implementer of the fix). Every program below spells a destructive call in a
//! place that is NOT positively inert: an interpolating or prefixed literal, an
//! unterminated quote, or next to dynamic execution / a real call. Each is Deny
//! at base (1e140b21) and must stay Deny after the string-only downgrade lands.
//! Programs are fed through quoted-delimiter heredocs so the shell passes them
//! verbatim.

#[path = "support/bounded.rs"]
mod bounded;

use std::process::{Command, Stdio};

fn decision(command: &str) -> (String, String) {
    let bin = env!("CARGO_BIN_EXE_blastguard");
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": { "command": command },
    })
    .to_string();
    let mut hook = Command::new(bin);
    hook.env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = bounded::run_bounded(&mut hook, payload.as_bytes(), bounded::HOOK_DEADLINE);
    assert_eq!(out.status.code(), Some(0), "hook must exit 0 ({command:?})");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if stdout.trim().is_empty() {
        return ("allow".to_string(), String::new());
    }
    let doc: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout not JSON for {command:?} ({e}): {stdout}"));
    let hso = doc.get("hookSpecificOutput");
    let d = hso
        .and_then(|h| h.get("permissionDecision"))
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("no permissionDecision for {command:?}: {stdout}"))
        .to_string();
    let r = hso
        .and_then(|h| h.get("permissionDecisionReason"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    (d, r)
}

fn assert_deny(cmd: &str) {
    let (d, r) = decision(cmd);
    assert_eq!(d, "deny", "must stay Deny: {cmd:?} (reason: {r})");
}

#[test]
fn python_fstring_with_brace_is_code() {
    assert_deny("python3 - <<'PY'\nx = 1\nprint(f\"{x} os.unlink( docs\")\nPY");
}

#[test]
fn python_raw_prefixed_literal_is_not_proven_inert() {
    assert_deny("python3 - <<'PY'\nx = r\"docs: os.unlink( removes a file\"\nprint(x)\nPY");
}

#[test]
fn python_bytes_prefixed_literal_is_not_proven_inert() {
    assert_deny("python3 - <<'PY'\nx = b\"docs: os.unlink( removes a file\"\nprint(x)\nPY");
}

#[test]
fn ruby_hash_brace_interpolation_is_code() {
    assert_deny("ruby - <<'RB'\nputs \"#{1} File.delete( docs\"\nRB");
}

#[test]
fn perl_at_brace_interpolation_is_code() {
    assert_deny("perl - <<'PL'\nprint \"@{[ 1 ]} unlink( docs\";\nPL");
}

#[test]
fn literal_with_dollar_paren_is_not_proven_inert() {
    assert_deny("python3 - <<'PY'\nx = \"$(true) os.unlink( docs\"\nprint(x)\nPY");
}

#[test]
fn backtick_literal_is_code() {
    assert_deny("perl - <<'PL'\nmy $x = `echo unlink( docs`;\nPL");
}

#[test]
fn unterminated_quote_is_not_proven_inert() {
    assert_deny("python3 - <<'PY'\nx = \"docs: os.unlink( removes a file\nprint(x)\nPY");
}

#[test]
fn inert_looking_literal_plus_getattr_stays_denied() {
    assert_deny(
        "python3 - <<'PY'\nimport os\nx = \"docs: os.unlink(\"\ngetattr(os, \"remove\")(\"/tmp/x.tmp\")\nPY",
    );
}

#[test]
fn inert_looking_literal_plus_real_call_stays_denied() {
    assert_deny(
        "python3 - <<'PY'\nimport os\nx = \"docs: os.unlink(\"\nos.unlink('/tmp/x.tmp')\nPY",
    );
}

#[test]
fn literal_passed_to_exec_stays_denied() {
    assert_deny("python3 - <<'PY'\nexec(\"import os; os.unlink('/tmp/x.tmp')\")\nPY");
}
