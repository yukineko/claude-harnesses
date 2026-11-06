// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Verifier finding (round 2) on the backlog 27795e6e fix, 2026-11-07, condukt
//! run run-20261107-000302-82136 t1, commit 697da241: the string-only
//! downgrade (Deny to Ask) relies on `strip_hash_comments(_, true)` blanking
//! quoted string contents. A REAL call written inside string interpolation
//! (python f-string replacement field, ruby `#{}`) is executed, yet it sits
//! between quote characters. The task requires a real destructive call to keep
//! its current verdict (Deny at base 1e140b21).

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
fn python_fstring_replacement_field_real_unlink_stays_denied() {
    let cmd = "python3 -c \"import os; print(f'{os.unlink(\\\"/tmp/x.tmp\\\")}')\"";
    assert_eq!(decision(cmd), "deny", "{cmd:?}");
}

#[test]
fn python_heredoc_fstring_real_unlink_stays_denied() {
    let cmd = "python3 - <<'PY'\nimport os\nprint(f\"{os.unlink('/tmp/x.tmp')}\")\nPY";
    assert_eq!(decision(cmd), "deny", "{cmd:?}");
}

#[test]
fn ruby_interpolation_real_delete_stays_denied() {
    let cmd = "ruby -e 'puts \"#{File.delete(\"/tmp/x.tmp\")}\"'";
    assert_eq!(decision(cmd), "deny", "{cmd:?}");
}
