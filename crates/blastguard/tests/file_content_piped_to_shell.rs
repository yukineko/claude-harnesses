// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Output that carries text from a file or from stdin, piped into a shell or
//! interpreter, runs a program blastguard cannot read. That must be Deny or
//! Ask, never Allow (cannot-determine resolves to the restrictive side).
//!
//! One test per command, one assertion per test. Each case runs with a fresh
//! HOME so the repeat-deny downgrade cannot change a verdict.

#[path = "support/bounded.rs"]
mod bounded;

use std::process::Command;

fn decision(tag: &str, command: &str) -> (String, String) {
    let home = std::env::temp_dir().join(format!(
        "blastguard-file-pipe-shell-{}-{tag}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).expect("mkdir home");
    let bin = env!("CARGO_BIN_EXE_blastguard");
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": { "command": command },
    })
    .to_string();
    let mut cmd = Command::new(bin);
    cmd.env("HOME", &home)
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env_remove("CLAUDE_CODE_SESSION_ID");
    let out = bounded::run_bounded(&mut cmd, payload.as_bytes(), bounded::HOOK_DEADLINE);
    assert_eq!(out.status.code(), Some(0), "hook must exit 0 ({command:?})");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let _ = std::fs::remove_dir_all(&home);
    if stdout.trim().is_empty() {
        eprintln!("[{tag}] allow");
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
    eprintln!("[{tag}] {d}: {r}");
    (d, r)
}

fn assert_deny_or_ask(tag: &str, cmd: &str) {
    let (d, r) = decision(tag, cmd);
    assert!(
        d == "deny" || d == "ask",
        "{cmd:?} must be denied or asked, got {d}: {r}"
    );
}

fn assert_allow(tag: &str, cmd: &str) {
    let (d, r) = decision(tag, cmd);
    assert_eq!(d, "allow", "{cmd:?} must be allowed, got {d}: {r}");
}

#[test]
fn grep_file_into_sh() {
    assert_deny_or_ask("grep_sh", r#"grep -E "a" f | sh"#);
}

#[test]
fn cat_file_into_sh() {
    assert_deny_or_ask("cat_sh", "cat f | sh");
}

#[test]
fn head_file_into_bash() {
    assert_deny_or_ask("head_bash", "head -n 5 f | bash");
}

#[test]
fn tail_file_into_sh() {
    assert_deny_or_ask("tail_sh", "tail f | sh");
}

#[test]
fn sed_file_into_sh() {
    assert_deny_or_ask("sed_sh", "sed -n 1p f | sh");
}

#[test]
fn awk_file_into_sh() {
    assert_deny_or_ask("awk_sh", "awk 1 f | sh");
}

#[test]
fn sort_file_into_sh() {
    assert_deny_or_ask("sort_sh", "sort f | sh");
}

#[test]
fn cut_file_into_sh() {
    assert_deny_or_ask("cut_sh", "cut -f1 f | sh");
}

#[test]
fn tr_redirected_file_into_sh() {
    assert_deny_or_ask("tr_sh", "tr a b < f | sh");
}

#[test]
fn grep_redirected_file_into_sh() {
    assert_deny_or_ask("grep_redir_sh", "grep x < f | sh");
}

#[test]
fn grep_file_into_python3() {
    assert_deny_or_ask("grep_py", r#"grep -E "a" f | python3"#);
}

#[test]
fn cat_file_into_env_sh() {
    assert_deny_or_ask("cat_env_sh", "cat f | env sh");
}

// Controls: the sink is not a shell or interpreter.
#[test]
fn control_grep_file_into_sort_allowed() {
    assert_allow("ctl_sort", r#"grep -E "a" f | sort"#);
}

#[test]
fn control_cat_file_into_wc_allowed() {
    assert_allow("ctl_wc", "cat f | wc -l");
}

// Pinned at today's verdict on main 0.2.202: ALLOW (no ask, no deny). The
// program text is a literal on the command line, so the stdin-exec ask does
// not fire for it. This pins current behaviour, not a ruling that it is right.
#[test]
fn echo_literal_into_sh_pinned() {
    assert_allow("echo_sh", r#"echo "ls" | sh"#);
}
