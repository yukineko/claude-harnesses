// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (verifier finding on backlog 27795e6e fix, 2026-11-07, condukt run
//! run-20261107-000302-82136 t1, commit 32e71333): the string-only-mention
//! downgrade (Deny to Ask) returned EARLY, before the rest of the program was
//! judged. Observed with the 32e71333 binary:
//!   `python3 -c "open('/etc/hosts','w').write('')"`                   deny
//!   `python3 -c "x='os.unlink('; open('/etc/hosts','w').write('')"`   ask
//! Adding a harmless string must never weaken the verdict of the rest of the
//! command (model.rs: a Deny found anywhere must outrank an Ask found anywhere).

#[path = "support/bounded.rs"]
mod bounded;

use std::process::{Command, Stdio};

fn decision(command: &str) -> String {
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
        return "allow".to_string();
    }
    let doc: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout not JSON for {command:?} ({e}): {stdout}"));
    doc.get("hookSpecificOutput")
        .and_then(|h| h.get("permissionDecision"))
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("no permissionDecision for {command:?}: {stdout}"))
        .to_string()
}

#[test]
fn control_unrecoverable_write_alone_is_denied() {
    let cmd = "python3 -c \"open('/etc/hosts','w').write('')\"";
    assert_eq!(decision(cmd), "deny", "{cmd:?}");
}

#[test]
fn string_only_mention_does_not_mask_a_later_unrecoverable_write() {
    let cmd = "python3 -c \"x='os.unlink('; open('/etc/hosts','w').write('')\"";
    assert_eq!(
        decision(cmd),
        "deny",
        "a string-only `os.unlink(` mention must not downgrade the Deny the rest of the program earns: {cmd:?}"
    );
}

#[test]
fn string_only_mention_in_a_heredoc_does_not_mask_a_later_unrecoverable_write() {
    let cmd = "python3 - <<'PY'\nx = \"docs: os.unlink( removes a file\"\nopen('/etc/hosts','w').write('')\nPY";
    assert_eq!(decision(cmd), "deny", "{cmd:?}");
}
