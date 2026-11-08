// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Pins for the user rulings of 2026-11-08: first 「stashは自分で消していいとしたい」
//! (Claude may delete stashes itself), then narrowed to 「drop のみ Allow」.
//!
//! `git stash drop` (bare, with a `stash@{N}` ref, and under a `-C DIR` global
//! prefix) is Allow. `git stash clear` stays Deny: refs/stash is shared across
//! every worktree of the repository, so clear also wipes other sessions'
//! stashes. Controls: `git stash pop` stays Allow; the untracked sweep forms
//! (`git stash -u`, `git stash push -u`), `git reset --hard` and
//! `git clean -fdx` stay refused.
//!
//! Every command is evaluated through the real hook binary with cwd set to a
//! fresh temp dir that is NOT a git checkout and NOT a managed worktree, so
//! the managed-checkout relaxations cannot be what makes a verdict Allow.

#[path = "support/bounded.rs"]
mod bounded;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn fresh_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "blastguard-stashruling-{}-{tag}",
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

fn assert_verdict(tag: &str, command: &str, want: &str, why: &str) {
    let dir = fresh_dir(tag);
    let (d, r) = verdict(&dir, command);
    assert_eq!(
        d, want,
        "{command:?} -> {d} (reason: {r}), want {want}: {why}"
    );
}

const RULING: &str = "user ruling 2026-11-08 「drop のみ Allow」 makes git stash drop Allow";

const CLEAR_RULING: &str = "user ruling 2026-11-08 「drop のみ Allow」 keeps git stash clear \
Deny: refs/stash is shared across worktrees, so clear wipes other sessions' stashes";

#[test]
fn stash_drop_is_allowed() {
    assert_verdict("drop", "git stash drop", "allow", RULING);
}

#[test]
fn stash_drop_with_stash_ref_is_allowed() {
    assert_verdict("drop-ref", "git stash drop 'stash@{1}'", "allow", RULING);
}

#[test]
fn stash_clear_stays_denied() {
    assert_verdict("clear", "git stash clear", "deny", CLEAR_RULING);
}

#[test]
fn stash_drop_under_dash_c_prefix_is_allowed() {
    assert_verdict("c-drop", "git -C /some/path stash drop", "allow", RULING);
}

#[test]
fn stash_drop_with_stash_ref_under_dash_c_prefix_is_allowed() {
    assert_verdict(
        "c-drop-ref",
        "git -C /some/path stash drop 'stash@{1}'",
        "allow",
        RULING,
    );
}

#[test]
fn stash_clear_under_dash_c_prefix_stays_denied() {
    assert_verdict(
        "c-clear",
        "git -C /some/path stash clear",
        "deny",
        CLEAR_RULING,
    );
}

#[test]
fn control_stash_pop_stays_allowed() {
    assert_verdict(
        "pop",
        "git stash pop",
        "allow",
        "control: git stash pop was already Allow and must stay so",
    );
}

#[test]
fn control_reset_hard_stays_denied() {
    assert_verdict(
        "reset-hard",
        "git reset --hard",
        "deny",
        "control: the stash ruling does not extend to discarding the working tree",
    );
}

#[test]
fn control_stash_untracked_sweep_stays_denied() {
    assert_verdict(
        "stash-u",
        "git stash -u",
        "deny",
        "control: the untracked sweep removes untracked files like git clean; the ruling \
covers deleting stashes, not sweeping the working tree",
    );
}

#[test]
fn control_stash_push_untracked_sweep_stays_denied() {
    assert_verdict(
        "stash-push-u",
        "git stash push -u",
        "deny",
        "control: the untracked sweep removes untracked files like git clean; the ruling \
covers deleting stashes, not sweeping the working tree",
    );
}

#[test]
fn control_clean_fdx_outside_safe_root_stays_denied() {
    // The fresh temp cwd is itself a blastguard "safe root" (a temp dir), where
    // a confined `git clean -fdx` is Ask, not Deny. Target a path outside every
    // safe root so the refusal under test is the unbounded one.
    assert_verdict(
        "clean-fdx-outside",
        "git -C /some/path clean -fdx",
        "deny",
        "control: git clean -fdx outside a safe root irreversibly deletes untracked and ignored \
files",
    );
}

#[test]
fn control_clean_fdx_in_safe_root_is_still_refused() {
    let dir = fresh_dir("clean-fdx-safe");
    let (d, r) = verdict(&dir, "git clean -fdx");
    assert!(
        d == "deny" || d == "ask",
        "\"git clean -fdx\" in a temp-dir safe root -> {d} (reason: {r}), want deny or ask: \
control: the stash ruling must not make git clean -fdx Allow"
    );
}
