#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (false-positive candidate, observed 2026-11-14 by the test author on
//! b0b4ac6e3 / blastguard 0.2.568 while probing a side finding of
//! run-20261114-111444-69657): the common commit form
//! `git commit -m "$(cat <<'EOF' ... EOF\n)"` whose QUOTED heredoc body holds a
//! Markdown backtick span is Ask in main / Deny in subagent with (verbatim):
//!   "a backtick inside a double-quoted argument of `git commit` is command
//!    substitution: the shell runs that span and silently deletes it from the
//!    stored text. Use single quotes (or escape the backtick) to keep it literal"
//! Observed (main / subagent):
//!   git commit -m "$(cat <<'EOF'\nfix: handle `a.rs` case\nEOF\n)"   ask   / deny  <- FP candidate
//!   git commit -m "$(cat <<'EOF'\nfix: handle a.rs case\nEOF\n)"     allow / allow (control)
//!   git commit -m "fix: handle `a.rs` case"                           ask   / deny  (genuine substitution; not asserted)
//!
//! Two readings:
//!   (a) defect (FP): the backtick sits in the body of a quoted heredoc, which
//!       bash never expands, and the output of `$(...)` is not re-evaluated for
//!       substitutions, so the stored message keeps the backtick literally.
//!       The warning's claim ("the shell runs that span") is false here.
//!   (b) intended: blastguard warns on any backtick within the double-quoted
//!       `-m` argument without checking whether it arrives via a quoted
//!       heredoc inside `$(...)`, accepting the over-approximation.
//!
//! Asserted: the quoted-heredoc-with-backtick shapes are Allow in main and
//! subagent mode. Control: the same form without a backtick is Allow.
//!
//! DOES NOT PROVE: that the plain double-quoted `-m "...`x`..."` warning stays
//! (observed Ask above, deliberately not asserted here), unquoted `<<EOF`
//! bodies (where backticks ARE expanded), or other git subcommands.
//!
//! RED written by an independent test author, not the implementer.

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
        "bg-gitmsgqheredoc-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "git-commit-msg-quoted-heredoc-backtick-20261114",
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

fn unallowed(cmds: &[&str], tag: &str) -> Vec<String> {
    let home = unique_home(tag);
    let mut bad = Vec::new();
    for cmd in cmds {
        for subagent in [false, true] {
            let (d, r) = run_as(cmd, &home, subagent);
            if d != "allow" {
                bad.push(format!("subagent={subagent} {cmd:?} => {d} ({r})"));
            }
        }
    }
    bad
}

#[test]
#[ignore = "RED: 2026-11-14 backtick in a quoted <<'EOF' body inside git commit -m \"$(cat ...)\" is Ask/Deny — remove the ignore when fixed"]
fn backtick_in_quoted_heredoc_commit_message_is_allow() {
    let cmds = [
        "git commit -m \"$(cat <<'EOF'\nfix: handle `a.rs` case\nEOF\n)\"",
        "git commit -m \"$(cat <<'EOF'\nfix: handle `f() { rm -f $L; }; L=x; x=$(f)`\nEOF\n)\"",
    ];
    let bad = unallowed(&cmds, "red");
    assert!(
        bad.is_empty(),
        "expected Allow for every shape:\n{}",
        bad.join("\n")
    );
}

#[test]
fn control_quoted_heredoc_commit_message_without_backtick_is_allow() {
    let cmds = ["git commit -m \"$(cat <<'EOF'\nfix: handle a.rs case\nEOF\n)\""];
    let bad = unallowed(&cmds, "ctl");
    assert!(
        bad.is_empty(),
        "control shapes must stay Allow:\n{}",
        bad.join("\n")
    );
}
