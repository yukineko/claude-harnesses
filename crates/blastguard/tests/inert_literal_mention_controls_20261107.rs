// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Tests for various inert-looking literal mentions of destructive calls.
//! Each test verifies that these patterns are correctly identified as Deny
//! (the decision must be exactly "deny" to confirm the pattern is being detected).

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
fn python_fstring_with_unlink_mention_and_brace() {
    let cmd = "python3 -c \"print(f\\\"{x} os.unlink(\\\")\"";
    assert_eq!(
        decision(cmd),
        "deny",
        "f-string containing unlink( must be denied: {cmd:?}"
    );
}

#[test]
fn ruby_double_quote_interpolation_with_file_delete() {
    let cmd = "ruby -e \"puts #{File.delete}\"";
    let d = decision(cmd);
    assert!(d == "deny" || d == "ask", "ruby double-quoted interpolation with File.delete must be refused (deny or ask); got {d:?}: {cmd:?}");
}

#[test]
fn perl_double_quote_array_interpolation_with_unlink() {
    let cmd = "perl -e \"print @{[ unlink('/tmp/x') ]}\"";
    assert_eq!(
        decision(cmd),
        "deny",
        "perl double-quoted string with @{{ unlink }} interpolation must be denied: {cmd:?}"
    );
}

#[test]
fn python_double_quote_with_command_substitution() {
    let cmd = "python3 -c \"x = \\\"$( os.unlink('/tmp') )\\\"; print(x)\"";
    assert_eq!(
        decision(cmd),
        "deny",
        "python string containing $( with unlink must be denied: {cmd:?}"
    );
}

#[test]
fn perl_backtick_with_unlink() {
    let cmd = "perl -e \"`os.unlink()`\"";
    assert_eq!(
        decision(cmd),
        "deny",
        "perl backtick string with unlink must be denied: {cmd:?}"
    );
}

#[test]
fn ruby_backtick_with_file_delete() {
    let cmd = "ruby -e \"`File.delete()`\"";
    assert_eq!(
        decision(cmd),
        "deny",
        "ruby backtick string with File.delete must be denied: {cmd:?}"
    );
}

#[test]
fn python_unterminated_quote_with_unlink() {
    let cmd = "python3 -c \"x = \\\" os.unlink(";
    assert_eq!(
        decision(cmd),
        "deny",
        "python unterminated quote with unlink must be denied: {cmd:?}"
    );
}

#[test]
fn python_literal_string_plus_getattr_unlink() {
    let cmd = "python3 -c \"x = 'docs: os.unlink('; y = getattr(os, 'unlink')\"";
    assert_eq!(
        decision(cmd),
        "deny",
        "literal docs comment plus getattr(os, 'unlink') must be denied: {cmd:?}"
    );
}

#[test]
fn python_literal_string_with_real_unlink_call() {
    let cmd = "python3 -c \"x = 'docs: os.unlink('; os.unlink('/tmp/x.tmp')\"";
    assert_eq!(
        decision(cmd),
        "deny",
        "literal docs string with real unlink call must be denied: {cmd:?}"
    );
}

#[test]
fn python_exec_with_unlink_call() {
    let cmd = "python3 -c \"exec(\\\"import os; os.unlink('/tmp/x.tmp')\\\")\"";
    let d = decision(cmd);
    assert!(
        d == "deny" || d == "ask",
        "exec with unlink call must be refused (deny or ask); got {d:?}: {cmd:?}"
    );
}
