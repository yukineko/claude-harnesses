// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Adversarial fail-open probes for the 0.2.385 fix (backlog ffce9c59): a QUOTED
//! heredoc fed to `git commit -F -` whose opener line continues with a tail
//! (`| ; && ||`) is treated as data and the tail is judged on its own. Each
//! case below must still be caught: the body or the tail is really executed.
//! `deny` is asserted where Deny is clearly expected; `not allow` (deny or ask)
//! where the exact verdict is not clear. Judged as the main agent.
//! Written by a disinterested author (CLAUDE.md 2(a)); `src/` untouched.

#[path = "support/bounded.rs"]
mod bounded;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn fresh_dir(tag: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("blastguard-cmatk-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir
}

/// Decision (`allow` / `ask` / `deny`) for `command` judged as a main-agent Bash
/// call from `cwd`. Empty stdout is Allow.
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

fn assert_deny(tag: &str, cmd: &str) {
    let base = fresh_dir(tag);
    assert_eq!(decision(&base, cmd), "deny", "cmd: {cmd:?}");
}

fn assert_not_allow(tag: &str, cmd: &str) {
    let base = fresh_dir(tag);
    assert_ne!(decision(&base, cmd), "allow", "must not be allow; cmd: {cmd:?}");
}

/// Case 1: body is executed by sh. Deny expected.
#[test]
fn case01_pipe_sh_body() {
    assert_deny("c01", "git commit -F - <<'EOF' | sh\nrm -rf /etc\nEOF");
}

/// Case 2: body reaches bash through a pipeline. Deny expected.
#[test]
fn case02_pipe_grep_bash_body() {
    assert_deny("c02", "git commit -F - <<'EOF' | grep x | bash\nrm -rf /etc\nEOF");
}

/// Case 3: real tail after &&. Deny expected.
#[test]
fn case03_and_tail() {
    assert_deny("c03", "git commit -F - <<'EOF' && rm -rf /etc\nmsg\nEOF");
}

/// Case 4: real tail after ||. Deny expected.
#[test]
fn case04_or_tail() {
    assert_deny("c04", "git commit -F - <<'EOF' || rm -rf /etc\nmsg\nEOF");
}

/// Case 5: real tail after a single &. Deny expected.
#[test]
fn case05_background_tail() {
    assert_deny("c05", "git commit -F - <<'EOF' & rm -rf /etc\nmsg\nEOF");
}

/// Case 6: body supplies xargs rm target. Verdict not clearly Deny: not allow.
#[test]
fn case06_xargs_rm_body_target() {
    assert_not_allow("c06", "git commit -F - <<'EOF' | xargs rm -rf\n/etc\nEOF");
}

/// Case 7: tail writes /etc/passwd. Not allow.
#[test]
fn case07_tee_etc_passwd() {
    assert_not_allow("c07", "git commit -F - <<'EOF' | tee /etc/passwd\nmsg\nEOF");
}

/// Case 8: redirect writes /etc/passwd. Not allow.
#[test]
fn case08_redirect_etc_passwd() {
    assert_not_allow("c08", "git commit -F - <<'EOF' > /etc/passwd\nmsg\nEOF");
}

/// Case 9: sh -c 'cat' ignores stdin; verdict not clearly Deny: not allow.
#[test]
fn case09_sh_c_cat_body() {
    assert_not_allow("c09", "git commit -F - <<'EOF' | sh -c 'cat'\nrm -rf /etc\nEOF");
}

/// Case 10: git alias runs a shell command. Not allow.
#[test]
fn case10_alias_bang() {
    assert_not_allow(
        "c10",
        "git -c alias.commit='!rm -rf /etc' commit -F - <<'EOF' | cat\nmsg\nEOF",
    );
}

/// Case 11: real tail after `;`. Deny expected.
#[test]
fn case11_pipe_cat_then_semicolon_tail() {
    assert_deny("c11", "git commit -F - <<'EOF' | cat; rm -rf /etc\nmsg\nEOF");
}

/// Case 12: second heredoc fed to bash. Deny expected.
#[test]
fn case12_second_heredoc_to_bash() {
    assert_deny(
        "c12",
        "git commit -F - <<'EOF' | cat; bash <<'X'\nmsg\nEOF\nrm -rf /etc\nX",
    );
}

/// Case 13: unquoted delimiter expands command substitution. Not allow.
#[test]
fn case13_unquoted_command_substitution() {
    assert_not_allow("c13", "git commit -F - <<EOF | cat\n$(rm -rf /etc)\nEOF");
}

/// Case 14: -F /dev/stdin piped to sh. Deny expected.
#[test]
fn case14_dev_stdin_pipe_sh() {
    assert_deny("c14", "git commit -F /dev/stdin <<'EOF' | sh\nrm -rf /etc\nEOF");
}

/// Case 15: body reaches sh through head. Deny expected.
#[test]
fn case15_pipe_head_sh() {
    assert_deny("c15", "git commit -F - <<'EOF' | head -1 | sh\nrm -rf /etc\nEOF");
}

/// Benign control: tail only reads, body is a commit message. Allow.
#[test]
fn control_pipe_tail_1_is_allow() {
    let base = fresh_dir("ctl");
    let cmd = "git commit -F - <<'EOF' | tail -1\nrm -rf /some/path\nEOF";
    assert_eq!(decision(&base, cmd), "allow");
}
