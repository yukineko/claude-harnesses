// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED reproduction of a fail-open (observed 2026-11-05 on main 2f9f927e,
//! blastguard 0.2.265).
//!
//! `git stash push --all` / `--include-untracked` / `-u` stash and REMOVE
//! untracked files (like `git clean`), so blastguard refuses those spellings.
//! But git also accepts any unambiguous abbreviation of a long option, and
//! bash brace expansion turns `{-u,}` into the single argv word `-u`:
//!
//! * `git stash push --al`        == `--all`               (git 2.50.1: untracked file removed)
//! * `git stash push --include-u` == `--include-untracked` (git 2.50.1: untracked file removed)
//! * `bash -c 'git stash push {-u,}'` == `git stash push -u` (untracked file removed)
//!
//! `is_stash_sweep_flag` matches exact spellings only and the argv splitter
//! does no brace expansion, so all three are allowed.
//!
//! The RED cases were `#[ignore]`d until the scanner judged these readings; the ignores are removed.

#[path = "support/bounded.rs"]
mod bounded;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn fresh_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "blastguard-stashabbrev-{}-{tag}",
        std::process::id()
    ));
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
    assert_eq!(
        out.status.code(),
        Some(0),
        "hook must exit 0 (cmd: {command:?}); stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
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

fn assert_not_allowed(tag: &str, command: &str) {
    let dir = fresh_dir(tag);
    let (d, r) = verdict(&dir, command);
    assert_ne!(
        d, "allow",
        "fail-open: {command:?} is an untracked-removing stash (git abbreviation / bash brace expansion) but was allowed ({r})"
    );
}

#[test]
fn abbreviated_all_is_not_allowed() {
    assert_not_allowed("abbr-al", "git stash push --al");
}

#[test]
fn abbreviated_include_untracked_is_not_allowed() {
    assert_not_allowed("abbr-incu", "git stash push --include-u");
}

#[test]
fn brace_expanded_dash_u_is_not_allowed() {
    assert_not_allowed("brace-u", "git stash push {-u,}");
}

/// Control: the exact long flag is already refused.
#[test]
fn control_exact_all_is_not_allowed() {
    assert_not_allowed("exact-all", "git stash push --all");
}

/// Control: a plain message-only stash stays allowed (the RED cases must not
/// be satisfied by refusing every `git stash push`).
#[test]
fn control_plain_stash_with_message_is_allowed() {
    let dir = fresh_dir("plain-msg");
    let command = "git stash push -m msg";
    let (d, r) = verdict(&dir, command);
    assert_eq!(d, "allow", "{command:?} must stay allowed, got {d}: {r}");
}

/// Verifier finding on 3e82109e (2026-11-07): a quoted / escaped `}` inside
/// the brace hides the comma from a first-`{`-to-first-`}` scan after quote
/// removal. Real bash expands `{-mx\},-u}` to the two words `-mx}` and `-u`,
/// i.e. `git stash push -mx} -u` (an untracked sweep).
#[test]
fn brace_with_escaped_close_brace_is_not_allowed() {
    assert_not_allowed("brace-esc", "git stash push {-mx\\},-u}");
}

/// Same shape with a single-quoted `}`: bash expands `{-mx'}',-u}` to
/// `-mx}` and `-u`.
#[test]
fn brace_with_quoted_close_brace_is_not_allowed() {
    assert_not_allowed("brace-quot", "git stash push {-mx'}',-u}");
}

/// Control: a stash ref `stash@{0}` is not a brace expansion (no comma, no
/// `..`), so popping it must stay allowed.
#[test]
fn control_stash_ref_brace_is_allowed() {
    let dir = fresh_dir("stash-ref");
    let command = "git stash pop stash@{0}";
    let (d, r) = verdict(&dir, command);
    assert_eq!(d, "allow", "{command:?} must stay allowed, got {d}: {r}");
}
