// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Fail-closed pins for the inline-eval word-boundary change (backlog
//! 1943843c, `interpreter_payloads_after`). Each command carries a real
//! destructive `rm -rf` and must stay non-allow, through the real binary.
//! These pin behaviour that already held before the change.

#[path = "support/bounded.rs"]
mod bounded;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn fresh_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("blastguard-iewb-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir
}

fn verdict(cwd: &Path, command: &str) -> (String, String) {
    let bin = env!("CARGO_BIN_EXE_blastguard");
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "cwd": cwd.to_string_lossy(),
        "tool_input": { "command": command },
    })
    .to_string();
    let mut hook = Command::new(bin);
    hook.current_dir(cwd)
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env("CLAUDE_PROJECT_DIR", cwd)
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = bounded::run_bounded(&mut hook, payload.as_bytes(), bounded::HOOK_DEADLINE);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    // exit 2 (blocked) is a non-allow too.
    if out.status.code() == Some(2) {
        return ("deny".to_string(), stdout);
    }
    assert_eq!(
        out.status.code(),
        Some(0),
        "hook must exit 0 or 2 (cmd: {command:?}); stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    if stdout.trim().is_empty() {
        return ("allow".to_string(), String::new());
    }
    let doc: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout is not JSON for {command:?} ({e}): {stdout}"));
    let hso = doc.get("hookSpecificOutput");
    let decision = hso
        .and_then(|h| h.get("permissionDecision"))
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("no permissionDecision for {command:?}, got: {stdout}"))
        .to_string();
    let reason = hso
        .and_then(|h| h.get("permissionDecisionReason"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    (decision, reason)
}

fn assert_not_allow(tag: &str, cmd: &str) {
    let dir = fresh_dir(tag);
    let (d, r) = verdict(&dir, cmd);
    assert_ne!(
        d, "allow",
        "{tag}: {cmd:?} must stay non-allow (reason: {r})"
    );
}

#[test]
fn perl_second_dash_e_with_system_rm_rf_stays_non_allow() {
    assert_not_allow("perl2e", r#"perl -e 'print 1;' -e 'system "rm -rf /x"'"#);
}

#[test]
fn python_c_real_newline_then_os_system_rm_rf_stays_non_allow() {
    assert_not_allow("pynl", "python3 -c \"import os\nos.system('rm -rf /x')\"");
}

#[test]
fn python_c_trailing_word_argument_rm_rf_stays_non_allow() {
    assert_not_allow("pytrail", r#"python3 -c "print(1)" 'rm -rf /x'"#);
}

#[test]
fn python_c_unclosed_quote_program_with_rm_rf_stays_non_allow() {
    assert_not_allow(
        "pyunclosed",
        r#"python3 -c "import os; os.system('rm -rf /x')"#,
    );
}

#[test]
fn bash_c_rm_rf_control_stays_non_allow() {
    assert_not_allow("bashc", "bash -c 'rm -rf /x'");
}
