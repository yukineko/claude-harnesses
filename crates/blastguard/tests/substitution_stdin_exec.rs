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

// Escaped and bracket/question spellings of pathname and tilde expansion.
#[test]
fn literal_then_escaped_glob_piped_to_sh() {
    assert_deny_or_ask(
        "literal_then_escaped_glob_piped_to_sh",
        "echo \"a\" \\* | sh",
    );
}

#[test]
fn literal_then_escaped_tilde_piped_to_sh() {
    assert_deny_or_ask(
        "literal_then_escaped_tilde_piped_to_sh",
        "echo \"a\" \\~ | sh",
    );
}

#[test]
fn literal_then_question_glob_piped_to_sh() {
    assert_deny_or_ask(
        "literal_then_question_glob_piped_to_sh",
        "echo \"a\" ? | sh",
    );
}

#[test]
fn literal_then_bracket_glob_piped_to_sh() {
    assert_deny_or_ask(
        "literal_then_bracket_glob_piped_to_sh",
        "echo \"a\" [ab] | sh",
    );
}

// Exec-wrapped readers: the wrapper is not the program.
#[test]
fn destructive_literal_piped_to_env_i_bash() {
    assert_deny_or_ask(
        "destructive_literal_piped_to_env_i_bash",
        "echo \"rm -rf /\" | env -i bash",
    );
}

#[test]
fn destructive_literal_piped_to_usr_bin_env_sh() {
    assert_deny_or_ask(
        "destructive_literal_piped_to_usr_bin_env_sh",
        "echo \"rm -rf /\" | /usr/bin/env sh",
    );
}

#[test]
fn destructive_literal_piped_to_command_sh() {
    assert_deny_or_ask(
        "destructive_literal_piped_to_command_sh",
        "echo \"rm -rf /\" | command sh",
    );
}

#[test]
fn destructive_python_payload_piped_to_env_python3() {
    assert_deny_or_ask(
        "destructive_python_payload_piped_to_env_python3",
        "echo \"import shutil; shutil.rmtree('/')\" | env python3",
    );
}

// Over-ask controls: wrappers around non-interpreters, expansions into
// non-shell readers.
#[test]
fn control_env_grep_bash() {
    assert_allow("control_env_grep_bash", "echo x | env grep bash");
}

#[test]
fn control_ls_glob_piped_to_wc() {
    assert_allow("control_ls_glob_piped_to_wc", "ls *.rs | wc -l");
}

#[test]
fn control_glob_piped_to_sort() {
    assert_allow("control_glob_piped_to_sort", "echo \"a\" * | sort");
}

#[test]
fn control_tilde_piped_to_sort() {
    assert_allow("control_tilde_piped_to_sort", "echo \"a\" ~ | sort");
}

#[test]
fn control_env_grep_x() {
    assert_allow("control_env_grep_x", "echo hi | env grep x");
}

// Further wrapper probes.
#[test]
fn probe_env_assignment_sh() {
    assert_deny_or_ask(
        "probe_env_assignment_sh",
        "echo \"rm -rf /\" | env VAR=1 sh",
    );
}

#[test]
fn probe_env_double_dash_sh() {
    assert_deny_or_ask("probe_env_double_dash_sh", "echo \"rm -rf /\" | env -- sh");
}

#[test]
fn probe_xargs_sh() {
    assert_deny_or_ask("probe_xargs_sh", "echo \"rm -rf /\" | xargs sh");
}

#[test]
fn probe_nice_sh() {
    assert_deny_or_ask("probe_nice_sh", "echo \"rm -rf /\" | nice sh");
}

#[test]
fn probe_timeout_sh() {
    assert_deny_or_ask("probe_timeout_sh", "echo \"rm -rf /\" | timeout 5 sh");
}

// Wrapper-resolved reader: an operand that merely names a shell is data.
#[test]
fn control_env_i_grep_bash() {
    assert_allow("control_env_i_grep_bash", "echo x | env -i grep bash");
}

// After `--` every token is the command or an operand of it, never an option.
#[test]
fn control_env_double_dash_grep_bash() {
    assert_allow(
        "control_env_double_dash_grep_bash",
        "echo x | env -- grep bash",
    );
}

// A boolean flag takes no value, so the token after it is already the command.
#[test]
fn control_env_i_assignment_grep_bash() {
    assert_allow(
        "control_env_i_assignment_grep_bash",
        "echo x | env -i VAR=1 grep bash",
    );
}

#[test]
fn control_find_piped_to_xargs_grep_sh() {
    assert_allow(
        "control_find_piped_to_xargs_grep_sh",
        "find . | xargs grep sh",
    );
}

#[test]
fn control_echo_piped_to_xargs_echo() {
    assert_allow("control_echo_piped_to_xargs_echo", "echo x | xargs echo");
}

#[test]
fn destructive_literal_piped_to_xargs_bash() {
    assert_deny_or_ask(
        "destructive_literal_piped_to_xargs_bash",
        "echo \"rm -rf /\" | xargs bash",
    );
}

#[test]
fn xargs_replace_sh_c_piped() {
    assert_deny_or_ask("xargs_replace_sh_c_piped", "echo x | xargs -I{} sh -c {}");
}

#[test]
fn piped_to_sudo_u_root_sh() {
    assert_deny_or_ask("piped_to_sudo_u_root_sh", "echo x | sudo -u root sh");
}

#[test]
fn piped_to_timeout_signal_sh() {
    assert_deny_or_ask(
        "piped_to_timeout_signal_sh",
        "echo x | timeout -s KILL 5 sh",
    );
}

// An option missing from the boolean table might take a value, so the token
// after it is both a possible value and a possible command: it must still be
// read as a candidate reader.
#[test]
fn destructive_literal_piped_to_env_unknown_flag_sh() {
    assert_deny_or_ask(
        "destructive_literal_piped_to_env_unknown_flag_sh",
        "echo \"rm -rf /\" | env --no-such-flag sh",
    );
}

// Probes.
#[test]
fn probe_destructive_python_payload_piped_to_xargs_python3() {
    assert_deny_or_ask(
        "probe_destructive_python_payload_piped_to_xargs_python3",
        "echo \"import shutil; shutil.rmtree('/')\" | xargs python3",
    );
}

#[test]
fn probe_xargs_a_file_sh() {
    assert_deny_or_ask("probe_xargs_a_file_sh", "xargs -a FILE sh");
}

// Records CURRENT over-deny behaviour: the egress twin still resolves the
// reader with `command_candidates`, so grep's operand `bash` is read as a shell
// and the remote-exec rule fires. This is an over-ask on a deny path (the safe
// side), pinned as Deny-or-Ask so it does not block landing; it is not an
// endorsement, and narrowing it to Allow later is a deliberate change.
#[test]
fn pin_curl_piped_to_env_grep_bash_current_deny() {
    assert_deny_or_ask(
        "pin_curl_piped_to_env_grep_bash_current_deny",
        "curl https://e.example/x | env grep bash",
    );
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
