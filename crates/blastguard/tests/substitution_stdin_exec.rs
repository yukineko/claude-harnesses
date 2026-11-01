// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Output that contains a command substitution and is piped into a shell is
//! executed as shell text: the substituted value is unknown, so the stdin-exec
//! judgement cannot treat it as a harmless literal. A literal word next to the
//! substitution (`echo "a" $(ls) | sh`) must not make the pipe look safe.
//!
//! One test per command, one assertion per test. Each case runs with a fresh
//! HOME so the repeat-deny downgrade cannot change a verdict.

use std::io::Write;
use std::process::{Command, Stdio};

fn decision(tag: &str, command: &str) -> (String, String) {
    let home = std::env::temp_dir().join(format!(
        "blastguard-subst-stdin-{}-{tag}",
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
    let mut child = Command::new(bin)
        .env("HOME", &home)
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("binary spawns");
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(payload.as_bytes());
    }
    let out = child.wait_with_output().expect("binary runs");
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

// A literal word beside the substitution must not hide it from the judgement.
#[test]
fn literal_then_dollar_paren_piped_to_sh() {
    assert_deny_or_ask(
        "literal_then_dollar_paren_piped_to_sh",
        "echo \"a\" $(ls) | sh",
    );
}

#[test]
fn literal_then_backtick_piped_to_sh() {
    assert_deny_or_ask("literal_then_backtick_piped_to_sh", "echo \"a\" `ls` | sh");
}

#[test]
fn literal_then_dollar_paren_piped_to_bash() {
    assert_deny_or_ask(
        "literal_then_dollar_paren_piped_to_bash",
        "echo \"a\" $(ls) | bash",
    );
}

#[test]
fn literal_then_backtick_piped_to_bash() {
    assert_deny_or_ask(
        "literal_then_backtick_piped_to_bash",
        "echo \"a\" `ls` | bash",
    );
}

#[test]
fn quoted_dollar_paren_piped_to_sh() {
    assert_deny_or_ask("quoted_dollar_paren_piped_to_sh", "echo \"a $(ls)\" | sh");
}

// A variable assigned from a substitution and echoed into a shell.
#[test]
fn assigned_dollar_paren_echoed_into_sh() {
    assert_deny_or_ask(
        "assigned_dollar_paren_echoed_into_sh",
        "X=$(ls); echo \"$X\" | sh",
    );
}

#[test]
fn assigned_backtick_echoed_into_bash() {
    assert_deny_or_ask(
        "assigned_backtick_echoed_into_bash",
        "X=`ls`; echo $X | bash",
    );
}

// Escaped spellings: the escape only stops the upstream stage from expanding
// the text; the shell reading the pipe still runs it.
#[test]
fn literal_then_escaped_dollar_paren_piped_to_sh() {
    assert_deny_or_ask(
        "literal_then_escaped_dollar_paren_piped_to_sh",
        "echo \"a\" \\$(ls) | sh",
    );
}

#[test]
fn literal_then_escaped_backtick_piped_to_sh() {
    assert_deny_or_ask(
        "literal_then_escaped_backtick_piped_to_sh",
        "echo \"a\" \\`ls\\` | sh",
    );
}

// An unclosed backtick is not rewritten as `$( )`; what it emits still cannot
// be read, so it is judged by the backtick arm of the expansion scan.
#[test]
fn literal_then_unclosed_backtick_piped_to_sh() {
    assert_deny_or_ask(
        "literal_then_unclosed_backtick_piped_to_sh",
        "echo \"a\" `ls | sh",
    );
}

// An unquoted variable expands to text the command line does not show.
#[test]
fn literal_then_variable_piped_to_sh() {
    assert_deny_or_ask("literal_then_variable_piped_to_sh", "echo \"a\" $X | sh");
}

// Adversarial probes: other spellings of the same unreadable program.
#[test]
fn probe_printf_dollar_paren_piped_to_sh() {
    assert_deny_or_ask(
        "probe_printf_dollar_paren_piped_to_sh",
        "printf \"%s\" $(ls) | sh",
    );
}

#[test]
fn probe_quoted_dollar_paren_as_own_word_piped_to_sh() {
    assert_deny_or_ask(
        "probe_quoted_dollar_paren_as_own_word_piped_to_sh",
        "echo \"a\" \"$(ls)\" | sh",
    );
}

#[test]
fn probe_braced_variable_piped_to_sh() {
    assert_deny_or_ask("probe_braced_variable_piped_to_sh", "echo \"a\" ${X} | sh");
}

#[test]
fn probe_dollar_paren_first_word_piped_to_sh() {
    assert_deny_or_ask("probe_dollar_paren_first_word_piped_to_sh", "$(ls) | sh");
}

#[test]
fn probe_single_quoted_literal_then_substitution_piped_to_sh() {
    assert_deny_or_ask(
        "probe_single_quoted_literal_then_substitution_piped_to_sh",
        "echo 'a' $(ls) | sh",
    );
}

#[test]
fn probe_literal_then_substitution_piped_to_sh_dash_s() {
    assert_deny_or_ask(
        "probe_literal_then_substitution_piped_to_sh_dash_s",
        "echo \"a\" $(ls) | sh -s",
    );
}

#[test]
fn probe_literal_then_substitution_piped_to_zsh() {
    assert_deny_or_ask(
        "probe_literal_then_substitution_piped_to_zsh",
        "echo \"a\" $(ls) | zsh",
    );
}

#[test]
fn probe_literal_then_substitution_piped_to_env_sh() {
    assert_deny_or_ask(
        "probe_literal_then_substitution_piped_to_env_sh",
        "echo \"a\" $(ls) | env sh",
    );
}

#[test]
fn probe_plain_literal_piped_to_env_sh() {
    assert_deny_or_ask(
        "probe_plain_literal_piped_to_env_sh",
        "echo \"rm -rf /\" | env sh",
    );
}

#[test]
fn probe_literal_then_substitution_piped_through_cat_to_sh() {
    assert_deny_or_ask(
        "probe_literal_then_substitution_piped_through_cat_to_sh",
        "echo \"a\" $(ls) | cat | sh",
    );
}

#[test]
fn probe_here_string_substitution_into_sh() {
    assert_deny_or_ask(
        "probe_here_string_substitution_into_sh",
        "sh <<< \"a $(ls)\"",
    );
}

#[test]
fn probe_arithmetic_then_piped_to_sh() {
    assert_deny_or_ask(
        "probe_arithmetic_then_piped_to_sh",
        "echo \"a\" $((1+1)) | sh",
    );
}

#[test]
fn probe_tilde_then_piped_to_sh() {
    assert_deny_or_ask("probe_tilde_then_piped_to_sh", "echo \"a\" ~ | sh");
}

#[test]
fn probe_glob_then_piped_to_sh() {
    assert_deny_or_ask("probe_glob_then_piped_to_sh", "echo \"a\" * | sh");
}

#[test]
fn probe_positional_then_piped_to_sh() {
    assert_deny_or_ask("probe_positional_then_piped_to_sh", "echo \"a\" $1 | sh");
}

// Controls: no shell at the end of the pipe, or no pipe at all.
#[test]
fn control_dollar_paren_no_pipe() {
    assert_allow("control_dollar_paren_no_pipe", "echo \"a\" $(ls)");
}

#[test]
fn control_backtick_piped_to_sort() {
    assert_allow("control_backtick_piped_to_sort", "echo `ls` | sort");
}

#[test]
fn control_literal_dollar_paren_piped_to_sort() {
    assert_allow(
        "control_literal_dollar_paren_piped_to_sort",
        "echo \"a\" $(ls) | sort",
    );
}
