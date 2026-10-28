// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Hypothesis under test: blastguard reads backtick prose inside a QUOTED
//! heredoc body as a redirect target.
//!
//! Observed 2026-10-27 (live hook), on
//! `cat > crates/overwatch/src/presence.rs <<'EOF' ... EOF` whose body held the
//! Rust doc-comment prose "/// corrupt => `Err`":
//!
//! ```text
//! '> `Err`' destroys the file's current contents and ``Err`` contains a shell
//! expansion, so blastguard cannot tell which file this names, let alone
//! whether its contents are recoverable
//! ```
//!
//! With a quoted delimiter (`<<'EOF'`) the shell neither expands the body nor
//! runs redirects in it: the body is data. The only real redirect is
//! `> crates/overwatch/src/presence.rs`, a not-yet-existing file in the
//! project.
//!
//! Differs from `heredoc_quoted_body_redirect_false_positive.rs` (which uses an
//! absolute temp-dir target and `x > \`y\``): here the target is RELATIVE to a
//! project cwd and the `>` is the second half of `=>`.
//!
//! Written by a disinterested author (CLAUDE.md §2(a)); `src/` untouched.
//!
//! Anti-vacuity controls:
//!   1. the same command with a body that holds no `>` is the baseline; the
//!      prose body must get the SAME decision, so any difference is pinned to
//!      the body prose and nothing else (target, cwd, verb);
//!   2. a genuine unquoted `> $X` redirect OUTSIDE the heredoc is still
//!      blocked/asked and its reason names the expansion, so this harness can
//!      observe the redirect-expansion analysis at all.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// A fresh temp project with `crates/overwatch/src/` present but
/// `presence.rs` absent, so the real redirect target has nothing to destroy.
fn fresh_project(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "blastguard-heredoc-doccomment-{}-{tag}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("crates/overwatch/src")).expect("mkdir project");
    assert!(
        !dir.join("crates/overwatch/src/presence.rs").exists(),
        "target must not exist"
    );
    dir
}

fn verdict(cwd: &std::path::Path, command: &str) -> (String, String) {
    let bin = env!("CARGO_BIN_EXE_blastguard");
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "cwd": cwd.to_string_lossy(),
        "tool_input": { "command": command },
    })
    .to_string();
    let mut child = Command::new(bin)
        .current_dir(cwd)
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env("CLAUDE_PROJECT_DIR", cwd)
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

const TARGET: &str = "crates/overwatch/src/presence.rs";

fn heredoc_write(body: &str) -> String {
    format!("cat > {TARGET} <<'EOF'\n{body}\nEOF")
}

/// Body prose shaped like the observed one: a Rust doc comment with `=>`
/// followed by a backtick span.
const PROSE_BODY: &str = "//! Presence ledger.\n\
/// Reads the ledger.\n\
/// corrupt => `Err`\n\
pub fn read() {}";

/// Control 1 body: identical apart from having no `>` anywhere.
const PLAIN_BODY: &str = "//! Presence ledger.\n\
/// Reads the ledger.\n\
/// corrupt gives `Err`\n\
pub fn read() {}";

#[test]
#[ignore = "RED repro: blastguard reads backtick prose inside a quoted heredoc body as a redirect target (run with -- --ignored)"]
fn quoted_heredoc_body_doc_comment_backtick_is_not_a_redirect_target() {
    let proj = fresh_project("prose");
    let (base_d, base_r) = verdict(&proj, &heredoc_write(PLAIN_BODY));
    let (d, r) = verdict(&proj, &heredoc_write(PROSE_BODY));
    assert!(
        !r.contains("`Err`"),
        "quoted heredoc body prose cited as a redirect target.\n\
         prose body -> {d}: {r}\n\
         control (no `>` in body) -> {base_d}: {base_r}"
    );
    assert_eq!(
        (d.as_str(), r.as_str()),
        (base_d.as_str(), base_r.as_str()),
        "body prose changed the verdict versus the no-`>` control"
    );
}

/// Control 1 on its own: the no-`>` baseline does not mention `Err` as a
/// redirect, so the assertion above compares against a clean reference.
#[test]
fn control_plain_body_baseline_does_not_cite_body_text() {
    let proj = fresh_project("plain");
    let (d, r) = verdict(&proj, &heredoc_write(PLAIN_BODY));
    assert!(
        !r.contains("Err"),
        "baseline already cites body text ({d}): {r}"
    );
}

/// Control 2: a genuine unquoted `> $X` redirect outside the heredoc IS
/// blocked or asked, and the reason names the expansion — proving this
/// harness observes the redirect-expansion analysis.
#[test]
fn control_real_unquoted_expansion_redirect_outside_heredoc_is_blocking() {
    let proj = fresh_project("expansion");
    let cmd = format!("cat > $X <<'EOF'\n{PLAIN_BODY}\nEOF");
    let (d, r) = verdict(&proj, &cmd);
    assert!(
        d == "deny" || d == "ask",
        "real `> $X` redirect not blocking: {d} ({r})"
    );
    assert!(
        r.contains("$X"),
        "reason does not name the real expansion target ({d}): {r}"
    );
}
