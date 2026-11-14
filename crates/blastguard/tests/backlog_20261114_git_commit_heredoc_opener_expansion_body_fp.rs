#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (false positive, observed 2026-11-14 at main 9bab9873e, blastguard
//! 0.2.594 built from that rev; first seen in run-20261114-163341-69351, task
//! worktree run-20261114-163341-69351-b793fddc, relay bgr-5f4e253e177a): a
//! QUOTED-delimiter `git commit -F - <<'EOF'` whose opener line holds a quoted
//! shell expansion in an option value (`--trailer "Condukt-Task: $TASK_ID"`)
//! keeps its body in the judged text, so a message line naming a protected
//! path is read as a command on that path. Verbatim reason (main: ask,
//! subagent: deny):
//!
//!   `second` is a command blastguard has no rule for, and .githooks/pre-push,
//!   is a protected gate/config path — blastguard cannot tell whether this
//!   reads it, rewrites it or removes it, and refuses to guess
//!
//! The body is git's stdin only; `"…$TASK_ID"` is one double-quoted word, the
//! value of `--trailer`, and cannot turn git into a reader that runs stdin.
//!
//! Observed splits (main session, 0.2.594):
//!   git commit -q -F - <<'EOF' + body                              allow
//!   git commit -q -F - --trailer "T: lit" <<'EOF' + body           allow
//!   git commit -q -F - --trailer "T: $X" <<'EOF' + body            ask   <- FP
//!   git commit -q -F - --author "$X" <<'EOF' + body                ask   <- FP
//!   git commit -q -F - --trailer "T: $X" <<'EOF' + body w/o path   allow
//!   git add -- a.py .githooks/pre-push && git commit … (no $)      allow
//!   git add -- a.py .githooks/pre-push (alone)                     allow
//! So neither `git add -- .githooks/pre-push` nor the line-initial word
//! `second` alone causes it; the `$` on the opener line does, and the
//! protected path in the body is what then gets judged.
//!
//! Suspected site (UNVERIFIED, not proven by this test):
//! crates/blastguard/src/detect/heredoc.rs ~1031-1035 `expansion_free`
//! (`!consumer.contains(['$', '`'])` / `!opener_line.contains(['$', '`'])`).
//!
//! Controls (GREEN at HEAD, not ignored): the same `$` opener with a later
//! line that RUNS the stored message (`sh .git/COMMIT_EDITMSG`) and a
//! destructive / protected-path body is not Allow.
//!
//! DOES NOT PROVE: that the fix belongs at the suspected site; anything about
//! unquoted expansions on the opener line (`--trailer $X`), which this test
//! does not assert.

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
        "bg-gcexp-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

/// Run the hook. Returns (decision, reason).
fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "git-commit-opener-expansion-20261114",
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

const PROTECTED_BODY: &str =
    "subject line\n\nThe ruling is read narrowly (no git worktree /\nsecond checkout). Prose in .githooks/pre-push, the docstrings\nis updated to match.\nEOF\n";

fn opener(option: &str) -> String {
    format!("git commit -q -F - {option} <<'EOF'\n")
}

// test-weakening-justified: new RED for an unfixed false positive (backlog
// pending, 2026-11-14); the ignore is removed in the fixing commit.
#[test]
#[ignore = "RED: <backlog-pending> 2026-11-14 quoted git commit -F - heredoc body judged when opener has a quoted $ option value; protected path in message read as command; remove the ignore when fixed"]
fn quoted_commit_heredoc_body_with_dollar_option_value_is_not_judged() {
    let home = unique_home("red");
    for option in [
        "--trailer \"Condukt-Task: $TASK_ID\"",
        "--author \"$AUTHOR\"",
    ] {
        let command = opener(option) + PROTECTED_BODY;
        for subagent in [false, true] {
            let (d, r) = run_as(&command, &home, subagent);
            assert!(
                !r.contains("protected gate/config path"),
                "option={option} subagent={subagent}: commit message line read as a command: {d} | {r}"
            );
            assert!(
                d != "deny" && d != "ask",
                "option={option} subagent={subagent}: {d} | {r}"
            );
        }
    }
}

// Sanity (GREEN at HEAD): the literal twin of the same commit is Allow, so the
// `$` on the opener line is what keeps the body judged.
#[test]
fn sanity_literal_option_value_commit_is_allowed() {
    let home = unique_home("lit");
    let command = opener("--trailer \"Condukt-Task: lit\"") + PROTECTED_BODY;
    for subagent in [false, true] {
        let (d, r) = run_as(&command, &home, subagent);
        assert_eq!(d, "allow", "subagent={subagent}: {d} | {r}");
    }
}

// Control: with a later line that runs the stored message, a destructive body
// stays judged and is not Allow.
#[test]
fn control_dollar_opener_body_run_from_commit_editmsg_is_not_allowed() {
    let home = unique_home("ctl");
    for body in [
        "subject\n\nrm -rf /etc\nEOF\n",
        "subject\n\necho x > .githooks/pre-push\nEOF\n",
    ] {
        let command =
            opener("--trailer \"Condukt-Task: $TASK_ID\"") + body + "sh .git/COMMIT_EDITMSG\n";
        for subagent in [false, true] {
            let (d, r) = run_as(&command, &home, subagent);
            assert_ne!(d, "allow", "body={body:?} subagent={subagent}: {d} | {r}");
        }
    }
}
