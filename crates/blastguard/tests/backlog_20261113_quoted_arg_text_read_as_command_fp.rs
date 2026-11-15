#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (false positive, observed 2026-11-13 at main f0854f26 in a subagent): text
//! `ln -sfn /tmp/x d && touch d/f` that is DATA (inside a quoted argument or a
//! heredoc body) is read as if it were a command, and a later redirect target
//! is denied: "'> /tmp/..': `\"a ln -sfn /tmp/x d` may create or rename an
//! entry on this path and is not modelled, so blastguard cannot tell which file
//! the write really lands on".
//!
//! Minimal repro found by probing (subagent mode): the ingredient is a
//! NEWLINE-separated statement list — a heredoc body (or quoted multi-line
//! text) containing the `ln -sfn ..` words, followed by a command with a
//! REDIRECT. Single-line quoted arguments (with or without a redirect, with
//! escaped quotes / `$`) and a quoted argument with a literal newline plus
//! redirect are Allow at HEAD (see the green control below).
//!
//! Asserted: the heredoc-body shapes are Allow in subagent mode.
//! Controls (GREEN at HEAD):
//!  - single-line quoted-argument shapes stay Allow (so the ingredient is the
//!    heredoc/newline body, not the quoting);
//!  - a heredoc whose body has no `ln` text plus the same redirect is Allow;
//!  - a REAL `ln -sfn /etc d && touch d/hosts` (and `echo x > d/hosts`) is NOT
//!    Allow (real ln is modelled; a real `ln -sfn /tmp/x d && touch d/f` is
//!    Allow at HEAD, so a system-dir target is used to be non-trivial).
//!
//! DOES NOT PROVE: that blastguard is right to deny the real command beyond
//! it staying not-Allow, nor anything about non-subagent mode.

#[path = "support/bounded.rs"]
mod bounded;

use std::process::{Command, Stdio};

struct Home(std::path::PathBuf);
impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn unique_home(tag: &str) -> Home {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "bg-qarg-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "quoted-arg-fp-20261113",
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
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if stdout.trim().is_empty() {
        return ("allow".into(), String::new());
    }
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let h = &v["hookSpecificOutput"];
    (
        h["permissionDecision"].as_str().unwrap_or("?").to_string(),
        h["permissionDecisionReason"]
            .as_str()
            .unwrap_or("")
            .to_string(),
    )
}

/// Heredoc body (data) containing the words, then a redirecting command.
const HEREDOC_DATA_SHAPES: &[&str] = &[
    "cat >> /tmp/blastguard-red6-f.txt <<'EOF'\n\"a ln -sfn /tmp/x d && touch d/f b\"\nEOF\ntrue > /tmp/blastguard-red6-out.txt",
    "cat >> /tmp/blastguard-red6-f.txt <<'EOF'\na ln -sfn /tmp/x d && touch d/f b\nEOF\ntrue > /tmp/blastguard-red6-out.txt 2>&1",
];

/// Single-line quoted shapes: Allow at HEAD (control).
const SINGLE_LINE_QUOTED: &[&str] = &[
    "echo \"a ln -sfn /tmp/x d && touch d/f b\"",
    "printf '%s' \"x && touch d/f\"",
    "echo 'ln -sfn /tmp/x d && touch d/f'",
    "echo \"a ln -sfn /tmp/x d && touch d/f b\" > /tmp/blastguard-red6-out.txt 2>&1",
    "true --notes \"x ln -sfn /tmp/x d && touch d/f y\" > /tmp/blastguard-red6-out.txt 2>&1",
    "echo add --notes \"fix \\\"x\\\" costs \\$5; ln -sfn /tmp/x d && touch d/f python3 -c ok\"",
    "backlog add t --notes \"line one\nx ln -sfn /tmp/x d && touch d/f y\" > /tmp/blastguard-red6-out.txt 2>&1",
];

// test-weakening-justified: new RED for an unfixed defect (backlog pending,
// 2026-11-13); the ignore is removed in the fixing commit.
#[test]
#[ignore = "RED: <backlog-pending> 2026-11-13 ln/touch words in a heredoc body are read as a command when a later command redirects — remove the ignore when fixed"]
fn heredoc_body_text_is_not_read_as_command() {
    let home = unique_home("red");
    let mut failures = Vec::new();
    for c in HEREDOC_DATA_SHAPES {
        let (d, r) = run_as(c, &home, true);
        if d != "allow" {
            failures.push(format!("{c:?} -> {d} | {r}"));
        }
    }
    assert!(
        failures.is_empty(),
        "{} heredoc-data command(s) were not Allow:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

// Control 1: single-line quoted shapes are Allow (the quoting is not the cause).
#[test]
fn control_single_line_quoted_shapes_are_allowed() {
    let home = unique_home("ctl1");
    for c in SINGLE_LINE_QUOTED {
        let (d, r) = run_as(c, &home, true);
        assert_eq!(d, "allow", "{c:?} -> {d} | {r}");
    }
}

// Control 2: same heredoc + redirect without the ln text is Allow.
#[test]
fn control_heredoc_without_ln_text_is_allowed() {
    let home = unique_home("ctl2");
    let c = "cat >> /tmp/blastguard-red6-f.txt <<'EOF'\nplain data line\nEOF\ntrue > /tmp/blastguard-red6-out.txt";
    let (d, r) = run_as(c, &home, true);
    assert_eq!(d, "allow", "{c:?} -> {d} | {r}");
}

// Control 3: a REAL ln that points at a system directory, followed by a write
// through it, is still denied (the ln is modelled when it is a real command).
// (A real `ln -sfn /tmp/x d && touch d/f` is Allow at HEAD, so the control
// targets /etc to be non-trivial.)
#[test]
fn control_real_ln_into_system_dir_then_write_is_not_allowed() {
    let home = unique_home("ctl3");
    for c in [
        "ln -sfn /etc d && touch d/hosts",
        "ln -sfn /etc d && echo x > d/hosts",
    ] {
        let (d, r) = run_as(c, &home, true);
        assert_ne!(d, "allow", "{c:?} -> {d} | {r}");
    }
}
