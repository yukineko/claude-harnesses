#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! FP RED (observed 2026-11-18, session 06c2bd77, subagent a611cdac32e08bddb,
//! blastguard 0.2.675): `cd <wt> && cat > crates/tdd/tests/<x>.rs <<'EOF' …
//! EOF` + `. "$HOME/.cargo/env"; cargo test …` was denied with
//! `git checkout -- . discards all working-tree changes` (rule_id
//! `git-checkout-dash-dot`). The only `git checkout … -- .` text is inside a
//! Rust doc comment of the QUOTED heredoc body written to a `.rs` file; no
//! shell word runs git. Memory ruling: a heredoc body is judged by its target
//! language; `.rs` is not shell.
//!
//! The exact command is the fixture `fixtures/backlog_20261118_rs_heredoc_checkout_fp.cmd`,
//! extracted byte-for-byte with `jq -j '.tool_input.command'` from
//! `~/.blastguard/blocks/blocks.jsonl` line 692 (sha256 of the fixture
//! c42b19daf11d0f26e390e20beeff8556fdb6a77f9478a8836fe23e6cb3715076, 10952 bytes).
//! The hook only judges it; nothing is executed.
//!
//! The command is > 8 KiB, so a `TooLong` Deny (ruling 2026-11-16) may be a
//! correct verdict of its own; the RED asserts only that the verdict is not
//! the `git checkout -- .` misreading.
//!
//! DOES NOT PROVE: which body line triggers it, the verdict for a shorter
//! body, or anything about unquoted heredocs.
//!
//! RED written by an independent test author, not the implementer.

#[path = "support/bounded.rs"]
mod bounded;

use std::process::{Command, Stdio};

const REFUSED: &str = include_str!("fixtures/backlog_20261118_rs_heredoc_checkout_fp.cmd");
const MISREAD: &str = "git checkout -- . discards all working-tree changes";

struct Home(std::path::PathBuf);
impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn unique_home(tag: &str) -> Home {
    let dir = std::env::temp_dir().join(format!("bg-rs-hd-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

/// `(decision, reason)`. Exit status first; only then is an empty stdout
/// Allow; a non-JSON stdout panics.
fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "rs-heredoc-checkout-text-fp-20261118",
        "cwd": home.0.join("wt"),
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": {"command": command},
    });
    if subagent {
        p["agent_id"] = serde_json::json!("test-subagent");
    }
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_blastguard"));
    cmd.env_clear()
        .env("HOME", &home.0)
        .env("PATH", "/usr/bin:/bin")
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = bounded::run_bounded(&mut cmd, p.to_string().as_bytes(), bounded::HOOK_DEADLINE);
    assert!(
        out.status.success(),
        "hook exited {:?}: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if stdout.trim().is_empty() {
        return ("allow".into(), String::new());
    }
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("hook stdout is not JSON ({e}): {stdout:?}"));
    let h = &v["hookSpecificOutput"];
    (
        h["permissionDecision"].as_str().unwrap_or("?").to_string(),
        h["permissionDecisionReason"]
            .as_str()
            .unwrap_or("")
            .to_string(),
    )
}

fn assert_not_misread(tag: &str, command: &str) {
    let home = unique_home(tag);
    for subagent in [false, true] {
        let (d, r) = run_as(command, &home, subagent);
        assert!(
            !(d != "allow" && r.contains(MISREAD)),
            "subagent={subagent}: .rs heredoc text read as git checkout: {d} ({r})"
        );
    }
}

/// RED: the exact logged command.
// test-weakening-justified: new RED for an observed FP (2026-11-18)
#[test]
fn logged_rs_heredoc_is_not_read_as_git_checkout() {
    assert_not_misread("red", REFUSED);
}

/// Minimal shape: the same doc-comment phrase alone in a quoted heredoc
/// written to a `.rs` file.
#[test]
fn minimal_rs_heredoc_checkout_phrase_is_not_read_as_git_checkout() {
    assert_not_misread(
        "min",
        "cat > x.rs <<'EOF'\n//! `git checkout <branch> -- .`) therefore gets those files dropped\nEOF",
    );
}
