// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! False-positive guard for the "shell-lexed reading" (word-initial `#` comments
//! stripped, backslash-escaped quotes neutralised, `<(..)`/`>(..)` bodies analysed
//! as code; most restrictive verdict wins). Benign commands that merely contain
//! `#`, quotes or process substitution must stay Allow. Judged as the main agent
//! (no agent_id). Verdict only, never message text.
//! Written by a disinterested author (CLAUDE.md §2(a)); `src/` untouched.

#[path = "support/bounded.rs"]
mod bounded;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn fresh_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("blastguard-shlex-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir
}

/// Decision for `command` judged as the main agent. Empty stdout is Allow.
fn decision(cwd: &Path, command: &str) -> String {
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
        .env_remove("BLASTGUARD_ASK")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = bounded::run_bounded(&mut hook, payload.as_bytes(), bounded::HOOK_DEADLINE);
    assert_eq!(
        out.status.code(),
        Some(0),
        "hook must exit 0 (cmd: {command:?}); stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if stdout.trim().is_empty() {
        return "allow".to_string();
    }
    let doc: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout is not JSON for {command:?} ({e}): {stdout}"));
    doc.get("hookSpecificOutput")
        .and_then(|h| h.get("permissionDecision"))
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("no permissionDecision for {command:?}, got: {stdout}"))
        .to_string()
}

fn assert_allow(tag: &str, command: &str) {
    let cwd = fresh_dir(tag);
    let d = decision(&cwd, command);
    assert_eq!(d, "allow", "benign command must be Allow: {command:?}");
}

fn assert_deny(tag: &str, command: &str) {
    let cwd = fresh_dir(tag);
    let d = decision(&cwd, command);
    assert_eq!(d, "deny", "must be Deny: {command:?}");
}

// ------------------------------------------------------------ Allow cases

#[test]
fn quoted_hash_in_double_quotes() {
    assert_allow("a1", r#"echo "a # b" && ls"#);
}

#[test]
fn adjacent_single_quotes_with_hash() {
    assert_allow("a2", r#"echo 'it''s # not a comment'; ls"#);
}

#[test]
fn param_length_expansion() {
    assert_allow("a3", "x=abc; echo ${#x}");
}

#[test]
fn arithmetic_base_literal() {
    assert_allow("a4", "echo $((16#ff))");
}

#[test]
fn grep_include_pattern() {
    assert_allow("a5", "grep -n '#include' src/a.c");
}

#[test]
fn backslash_escaped_hash() {
    assert_allow("a6", r"echo foo\#bar; ls");
}

#[test]
fn quoted_heredoc_hash_and_quote_body() {
    assert_allow("a7", "cat <<'EOF'\n# heading with 'quote\nEOF");
}

#[test]
fn diff_process_substitution() {
    assert_allow("a8", "diff <(sort a.txt) <(sort b.txt)");
}

#[test]
fn case_statement_inside_command_substitution() {
    assert_allow("a9", r#"y=$(case "$x" in a) echo a;; esac); echo "$y""#);
}

#[test]
fn arithmetic_shift() {
    assert_allow("a10", "echo $((1<<3))");
}

// --------------------------------------------------------------- Controls

#[test]
fn control_comment_with_apostrophe_then_rm_is_deny() {
    assert_deny("c1", "ls # it's\nrm -rf /etc");
}

#[test]
fn control_process_substitution_rm_is_deny() {
    assert_deny("c2", "cat <(rm -rf /etc)");
}

#[test]
fn control_plain_rm_is_deny() {
    assert_deny("c3", "rm -rf /etc");
}
