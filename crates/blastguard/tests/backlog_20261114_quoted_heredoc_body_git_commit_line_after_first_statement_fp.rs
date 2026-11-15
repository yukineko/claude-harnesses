#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (false-positive candidate, observed 2026-11-14 by the test author: the
//! installed blastguard hook denied the author's own `git commit -q -F -
//! <<'EOF'` in subagent mode; then minimised against the b0b4ac6e3 /
//! blastguard 0.2.568 build): a QUOTED here-document body line that merely
//! reads like `git commit -m "`x`"` is judged as a real `git commit` with a
//! backtick in a double-quoted argument — but only when the heredoc-carrying
//! command is NOT the first statement of the command line. Reason (verbatim):
//!   "a backtick inside a double-quoted argument of `git commit` is command
//!    substitution: the shell runs that span and silently deletes it from the
//!    stored text. Use single quotes (or escape the backtick) to keep it literal"
//! Observed (main / subagent), body = `git commit -m "`x`"`:
//!   true && cat <<'EOF'\n<body>\nEOF                       ask   / deny  <- FP candidate
//!   true && git commit -F - <<'EOF'\n<body>\nEOF           ask   / deny  <- FP candidate
//!   true; git commit -F - <<'EOF'\n<body>\nEOF             ask   / deny  <- FP candidate
//!   git commit -F - <<'EOF'\n<body>\nEOF                   allow / allow (control: first statement)
//!   git add a && git commit -F - <<'EOF'\n<body>\nEOF      allow / allow (observed, not asserted)
//!   true && git commit -F - <<'EOF'\ngit commit -m "x"\nEOF   allow / allow (control: no backtick)
//!   true && git commit -F - <<'EOF'\nsee -m "`x`"\nEOF        allow / allow (observed, not asserted)
//! This plausibly explains run item 4 (`git commit -F - <<'EOF'` body with
//! backtick shell fragments denied in subagent): the run's command was a
//! chain, and a lone heredoc does not reproduce it. Not proven.
//!
//! Two readings:
//!   (a) defect: the quoted heredoc body is data (never expanded nor run), so
//!       judging a body line as a `git commit` command is a false positive; the
//!       dependence on statement position points at a body/command boundary
//!       error after the first statement.
//!   (b) intended: blastguard deliberately scans heredoc bodies for risky-looking
//!       command text. The first-statement control being Allow contradicts a
//!       deliberate rule, which points to (a).
//!
//! Asserted: the three FP shapes are Allow in main and subagent mode.
//! Control: the same body as the first statement, and the no-backtick body
//! after `true &&`, are Allow.
//!
//! DOES NOT PROVE: why `git add a &&` as the first statement does not trigger
//! it, other risky-looking body lines, unquoted heredocs, or that a genuine
//! `true && git commit -m "`x`"` stays Ask.
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
        "bg-heredocbodypos-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "heredoc-body-git-commit-line-20261114",
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
#[ignore = "RED: 2026-11-14 quoted heredoc body line `git commit -m \"`x`\"` is judged as a command after the first statement — remove the ignore when fixed"]
fn quoted_heredoc_body_git_commit_line_after_first_statement_is_allow() {
    let cmds = [
        "true && cat <<'EOF'\ngit commit -m \"`x`\"\nEOF",
        "true && git commit -F - <<'EOF'\ngit commit -m \"`x`\"\nEOF",
        "true; git commit -F - <<'EOF'\ngit commit -m \"`x`\"\nEOF",
    ];
    let bad = unallowed(&cmds, "red");
    assert!(
        bad.is_empty(),
        "expected Allow for every shape:\n{}",
        bad.join("\n")
    );
}

#[test]
fn control_quoted_heredoc_body_as_first_statement_or_without_backtick_is_allow() {
    let cmds = [
        "git commit -F - <<'EOF'\ngit commit -m \"`x`\"\nEOF",
        "true && git commit -F - <<'EOF'\ngit commit -m \"x\"\nEOF",
    ];
    let bad = unallowed(&cmds, "ctl");
    assert!(
        bad.is_empty(),
        "control shapes must stay Allow:\n{}",
        bad.join("\n")
    );
}
