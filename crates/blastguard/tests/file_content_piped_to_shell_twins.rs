// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Twins of `file_content_piped_to_shell.rs`, written by a verifier that did
//! not write the fix. The fix stopped treating the upstream stage's quoted
//! words as the whole program when that stage can emit more than its own
//! arguments. Each case below is another upstream shape that emits text the
//! command line does not show, piped into a shell or interpreter. Each must be
//! Deny or Ask, never Allow (cannot-determine resolves to the restrictive
//! side).
//!
//! One test per command, one assertion per test. Each case runs with a fresh
//! HOME so the repeat-deny downgrade cannot change a verdict.

#[path = "support/bounded.rs"]
mod bounded;

use std::process::Command;

fn decision(tag: &str, command: &str) -> (String, String) {
    let home = std::env::temp_dir().join(format!(
        "blastguard-file-pipe-twins-{}-{tag}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).expect("mkdir home");
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": { "command": command },
    })
    .to_string();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_blastguard"));
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
fn grep_fixed_string_file_into_sh() {
    assert_deny_or_ask("grep_F_sh", r#"grep -F "a" f | sh"#);
}

#[test]
fn grep_single_quoted_file_into_sh() {
    assert_deny_or_ask("grep_sq_sh", "grep 'a' f | sh");
}

#[test]
fn egrep_file_into_sh() {
    assert_deny_or_ask("egrep_sh", r#"egrep "a" f | sh"#);
}

#[test]
fn rg_file_into_sh() {
    assert_deny_or_ask("rg_sh", r#"rg "a" f | sh"#);
}

#[test]
fn jq_file_into_sh() {
    assert_deny_or_ask("jq_sh", "jq -r . f | sh");
}

// `cat` echoes a file, not its arguments: a quoted word is a file name there,
// so it must not be read as the program.
#[test]
fn cat_quoted_file_name_into_sh() {
    assert_deny_or_ask("cat_dq_sh", r#"cat "f" | sh"#);
}

#[test]
fn cat_single_quoted_file_name_into_python3() {
    assert_deny_or_ask("cat_sq_py", "cat 'f' | python3");
}

#[test]
fn printf_of_command_substitution_of_file_into_sh() {
    assert_deny_or_ask("printf_cat_sh", r#"printf '%s\n' "$(cat f)" | sh"#);
}

#[test]
fn subshell_cat_file_into_sh() {
    assert_deny_or_ask("subshell_cat_sh", "(cat f) | sh");
}

#[test]
fn brace_group_cat_file_into_sh() {
    assert_deny_or_ask("brace_cat_sh", "{ cat f; } | sh");
}

#[test]
fn env_wrapped_grep_file_into_sh() {
    assert_deny_or_ask("env_grep_sh", r#"env grep "a" f | sh"#);
}

#[test]
fn grep_file_into_env_python3() {
    assert_deny_or_ask("grep_env_py", r#"grep "a" f | env python3"#);
}

// Pins at the verdict the fixed code gives today: ALLOW. Here the program text
// is a literal on the command line (`echo` emits only its arguments), so there
// is no hidden text for the stdin-exec ask to cover. This pins current
// behaviour, not a ruling that Allow is right. If the verdict on `echo | sh`
// is ever deliberately changed, these two change with it.
#[test]
fn echo_literal_a_into_sh_pinned() {
    assert_allow("echo_a_sh", r#"echo "a" | sh"#);
}

#[test]
fn echo_literal_ls_into_sh_pinned() {
    assert_allow("echo_ls_sh", r#"echo "ls" | sh"#);
}

// Same pin behind an exec wrapper: `env` / `command` do not change what the
// emitter emits, so the literal is still the whole program.
#[test]
fn env_wrapped_echo_literal_into_sh_pinned() {
    assert_allow("env_echo_sh", r#"env echo "ls" | sh"#);
}

#[test]
fn command_wrapped_printf_literal_into_sh_pinned() {
    assert_allow("command_printf_sh", r#"command printf "ls" | sh"#);
}
