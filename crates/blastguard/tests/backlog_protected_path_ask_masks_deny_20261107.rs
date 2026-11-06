// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro (2026-11-07, found while verifying backlog 27795e6e in condukt run
//! run-20261107-000302-82136, reported by the t1 worker; observed with the
//! blastguard binary built at 697da241 and at the deployed 0.2.269):
//!   `python3 -c "open('/etc/hosts','w').write('')"`                       deny
//!   `python3 -c "x='.githooks'; open('/etc/hosts','w').write('')"`        ask
//! In `interpreter_program_verdict` the per-literal loop returns
//! `Decision::ask(... protected gate/config path ...)` as soon as a literal
//! names a protected path, before the unrecoverable-write probe runs on the
//! remaining literals. A Deny found anywhere on a command line must outrank an
//! Ask found anywhere else (model.rs). Two readings: (a) defect — naming a
//! protected path downgrades an unrelated irreversible write; (b) intended —
//! "protected path" was meant to be the final word. Tell apart: (b) would make
//! adding any protected-path string a way to turn a Deny into an Ask.

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
#[ignore = "backlog 7483e85b: protected-path literal Ask masks a later unrecoverable-write Deny (RED)"]
fn protected_path_literal_does_not_mask_a_later_unrecoverable_write() {
    let cmd = "python3 -c \"x='.githooks'; open('/etc/hosts','w').write('')\"";
    assert_eq!(
        decision(cmd),
        "deny",
        "naming a protected path must not downgrade the Deny the rest of the program earns: {cmd:?}"
    );
}
