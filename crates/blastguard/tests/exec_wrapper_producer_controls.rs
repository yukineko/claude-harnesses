// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Controls for the bare-wrapper rule of `b5.rs::bare_wrapped_command`: only a
//! BARE `env CMD` (no option, no `NAME=V` operand) and a BARE `command CMD`
//! (no `-p` / `-v` / `-V`) read as `CMD`, so `env echo "ls" | sh` is the
//! program `ls`. Every other wrapper shape keeps the restrictive reading: its
//! program is not the literal, so the line must be Deny or Ask, never Allow.
//!
//! One test per command, one assertion per test. Each case runs with a fresh
//! HOME so the repeat-deny downgrade cannot change a verdict.

#[path = "support/bounded.rs"]
mod bounded;

use std::process::Command;

fn decision(tag: &str, command: &str) -> (String, String) {
    let home = std::env::temp_dir().join(format!(
        "blastguard-exec-wrapper-controls-{}-{tag}",
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

fn assert_deny(tag: &str, cmd: &str) {
    let (d, r) = decision(tag, cmd);
    assert_eq!(d, "deny", "{cmd:?} must be denied, got {d}: {r}");
}

// `env` with a `NAME=V` operand: the operand is an assignment, not the command.
#[test]
fn env_assignment_echo_literal_into_sh() {
    assert_deny_or_ask("env_assign_sh", r#"env VAR=x echo "ls" | sh"#);
}

// `env -i`: an option, so the word after `env` is not the command.
#[test]
fn env_ignore_env_echo_literal_into_sh() {
    assert_deny_or_ask("env_i_sh", r#"env -i echo "ls" | sh"#);
}

// `env -S`: the option splits its operand as a command line of its own.
#[test]
fn env_split_string_echo_literal_into_sh() {
    assert_deny_or_ask("env_S_sh", r#"env -S "echo ls" | sh"#);
}

// `command -p`: an option, so the word after `command` is not the command.
#[test]
fn command_p_echo_literal_into_sh() {
    assert_deny_or_ask("command_p_sh", r#"command -p echo "ls" | sh"#);
}

// `command -v` prints a path instead of running the command.
#[test]
fn command_v_echo_literal_into_sh() {
    assert_deny_or_ask("command_v_sh", r#"command -v echo "ls" | sh"#);
}

// `command -V` describes the command instead of running it.
#[test]
fn command_capital_v_printf_literal_into_sh() {
    assert_deny_or_ask("command_V_sh", r#"command -V printf "ls" | sh"#);
}

// Two wrappers in a row: only one wrapper is unwrapped, so `command` is not a
// producer.
#[test]
fn env_then_command_echo_literal_into_sh() {
    assert_deny_or_ask("env_command_sh", r#"env command echo "ls" | sh"#);
}

// The unwrapped producer is judged like the bare one: a destructive unquoted
// `echo` behind a bare wrapper stays Deny (it was Deny before the unwrap).
#[test]
fn env_echo_unquoted_rm_into_bash_denied() {
    assert_deny("env_echo_rm_bash", "env echo rm -rf /etc | bash");
}

#[test]
fn command_echo_unquoted_rm_into_bash_denied() {
    assert_deny("command_echo_rm_bash", "command echo rm -rf /etc | bash");
}

// A destructive literal behind a bare wrapper, read through a process
// substitution: the same producer rule feeds `bash <( … )`.
#[test]
fn command_echo_quoted_rm_procsub_bash_denied() {
    assert_deny(
        "command_echo_procsub_bash",
        "bash <(command echo 'rm -rf /etc')",
    );
}

#[test]
fn env_printf_quoted_rm_into_sh_denied() {
    assert_deny("env_printf_rm_sh", "env printf '%s' 'rm -rf /etc' | sh");
}

// Verifier-independent probes (written by the integrator, not the
// implementer). `env CMD` resolves CMD through PATH, unlike the builtin echo,
// so a PATH rewrite earlier on the line makes the producer unknown.
#[test]
fn path_rewrite_then_env_echo_into_sh_not_allow() {
    assert_deny_or_ask(
        "path_env_echo",
        r#"export PATH=/tmp/evil; env echo "ls" | sh"#,
    );
}

#[test]
fn path_assign_then_env_echo_into_sh_not_allow() {
    assert_deny_or_ask(
        "path_assign_env_echo",
        r#"PATH=/tmp/evil; env echo "ls" | sh"#,
    );
}

// The wrapper itself redefined as a function on the same line.
#[test]
fn env_function_shadow_into_sh_not_allow() {
    assert_deny_or_ask(
        "env_fn_shadow",
        r#"env(){ printf 'rm -rf /tmp/x'; }; env echo "ls" | sh"#,
    );
}

#[test]
fn command_function_shadow_into_sh_not_allow() {
    assert_deny_or_ask(
        "command_fn_shadow",
        r#"command(){ printf 'rm -rf /tmp/x'; }; command printf "ls" | sh"#,
    );
}

// `--` ends options; the word after the wrapper starts with `-`, so it stays
// restrictive.
#[test]
fn env_double_dash_echo_into_sh_not_allow() {
    assert_deny_or_ask("env_dd", r#"env -- echo "ls" | sh"#);
}

#[test]
fn env_echo_unquoted_var_into_sh_not_allow() {
    assert_deny_or_ask("env_echo_var", r#"env echo $X | sh"#);
}

// Round-2 probes (integrator): PATH or the wrapper's lookup changed by
// something other than an assignment word.
#[test]
fn read_path_then_env_echo_into_sh_not_allow() {
    assert_deny_or_ask(
        "read_path",
        r#"read PATH <<< /tmp/evil; env echo "ls" | sh"#,
    );
}

#[test]
fn for_path_then_env_echo_into_sh_not_allow() {
    assert_deny_or_ask(
        "for_path",
        r#"for PATH in /tmp/evil; do env echo "ls" | sh; done"#,
    );
}

#[test]
fn hash_p_then_env_echo_into_sh_not_allow() {
    assert_deny_or_ask("hash_p", r#"hash -p /tmp/evil/x env; env echo "ls" | sh"#);
}

#[test]
fn printf_v_path_then_env_echo_into_sh_not_allow() {
    assert_deny_or_ask(
        "printf_v_path",
        r#"printf -v PATH /tmp/evil; env echo "ls" | sh"#,
    );
}

#[test]
fn source_then_env_echo_into_sh_not_allow() {
    assert_deny_or_ask("source_x", r#"source ./x; env echo "ls" | sh"#);
}
