// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro of a blastguard false positive observed 2026-11-07 (main agent,
//! blastguard 0.2.378): a commit whose message is fed through a quoted heredoc
//! on stdin, with commands following the opener line, was refused with
//! `PreToolUse:Bash hook error: recursive rm (-r) can delete an entire
//! directory tree`. The message body only MENTIONED `rm -rf /some/path`.
//!
//! A quoted heredoc fed to `git commit -F -` is commit-message DATA and is never
//! run, so these must be Allow (non-protected cwd, main agent).
//! Suspected cause: a command after the opener line makes blastguard scan the
//! body as code (README.ja.md documents scanning the body when commands follow
//! the heredoc). Not asserted here; only verdicts are.
//!
//! Judged as the main agent (no `agent_id`). Assertions are verdicts only.
//! Written by a disinterested author (CLAUDE.md §2(a)); `src/` untouched.
//! Do NOT weaken a test to make it pass; remove the `#[ignore]` on a RED test
//! when fixed.

#[path = "support/bounded.rs"]
mod bounded;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn fresh_dir(tag: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("blastguard-commitmsg-{}-{tag}", std::process::id()));
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

// ---------------------------------------------------------------- RED targets

/// TARGET: the exact observed shape.
#[test]
#[ignore = "backlog ffce9c59: open defect, remove ignore when fixed"]
fn exact_observed_commit_message_heredoc_is_allow() {
    let base = fresh_dir("exact");
    let wt = base.display();
    let cmd = format!(
        "cd {wt} && git add a.rs && git commit -q -F - <<'EOF' 2>&1 | grep -E 'blocked|green' ; git log --oneline -1\n\
fix: subject\n\
\n\
- the shape is now the same Deny as rm -rf /some/path.\n\
\n\
Co-Authored-By: X <x@example.com>\n\
EOF"
    );
    assert_eq!(
        decision(&base, &cmd),
        "allow",
        "commit message body is data"
    );
}

/// TARGET: minimal, a pipe after the opener.
#[test]
#[ignore = "backlog ffce9c59: open defect, remove ignore when fixed"]
fn commit_message_heredoc_piped_is_allow() {
    let base = fresh_dir("pipe");
    let cmd = "git commit -F - <<'EOF' | cat\nrm -rf /some/path\nEOF";
    assert_eq!(decision(&base, cmd), "allow", "commit message body is data");
}

/// TARGET: minimal, a `;` command after the opener.
#[test]
#[ignore = "backlog ffce9c59: open defect, remove ignore when fixed"]
fn commit_message_heredoc_semicolon_cmd_is_allow() {
    let base = fresh_dir("semi");
    let cmd = "git commit -F - <<'EOF'; git log -1\nrm -rf /some/path\nEOF";
    assert_eq!(decision(&base, cmd), "allow", "commit message body is data");
}

// ------------------------------------------------- controls (must pass now)

/// CONTROL: nothing after the opener is Allow.
#[test]
fn control_commit_message_heredoc_alone_is_allow() {
    let base = fresh_dir("c-alone");
    let cmd = "git commit -F - <<'EOF'\nrm -rf /some/path\nEOF";
    assert_eq!(decision(&base, cmd), "allow");
}

/// CONTROL: a heredoc fed to bash is executed.
#[test]
fn control_bash_heredoc_rm_etc_is_deny() {
    let base = fresh_dir("c-bash");
    let cmd = "bash <<'EOF'\nrm -rf /etc\nEOF";
    assert_eq!(decision(&base, cmd), "deny");
}

/// CONTROL: a heredoc piped into sh is executed.
#[test]
fn control_heredoc_piped_to_sh_is_deny() {
    let base = fresh_dir("c-sh");
    let cmd = "cat <<'EOF' | sh\nrm -rf /etc\nEOF";
    assert_eq!(decision(&base, cmd), "deny");
}

/// CONTROL: a real command after the opener is judged.
#[test]
fn control_real_rm_after_opener_is_deny() {
    let base = fresh_dir("c-real");
    let cmd = "git commit -F - <<'EOF'; rm -rf /etc\nmsg\nEOF";
    assert_eq!(decision(&base, cmd), "deny");
}
