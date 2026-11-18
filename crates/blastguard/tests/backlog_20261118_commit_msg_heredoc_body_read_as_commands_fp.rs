#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! FP RED (observed 2026-11-18, relay bgr-0dd8923dc0f3, subagent deny): a
//! commit whose message is a QUOTED heredoc fed to `git commit -q -F -` as
//! the 4th statement of an `&&` chain (`cd … && . "$HOME/.cargo/env" &&
//! rustfmt --check … && git add … && git commit -q -F - <<'EOF' … EOF`) had
//! its message lines judged as command lines. Verbatim reason (subagent):
//! "`blastguard:` is not a command blastguard recognises, and neither is
//! what follows it — … `OtherWorktree` is not a command blastguard
//! recognises, and what follows it parses as a destructive command line —
//! blastguard cannot tell whether `OtherWorktree` runs it". A quoted heredoc
//! body is data (user ruling 2026-11-17 on d292ce6e). The main session ran the
//! same command unblocked (exit 0, commit 23f5a3211).
//!
//! Not a duplicate of: 3ab6109f (needs a quoted `$` option value on the
//! opener — none here), d292ce6e (column-0 backtick — no backtick in this
//! body), 648a6641 (`;` before a backtick span — no backtick), bd2a44e0
//! (`"$(cat <<'EOF' …)"` — not this form), 2d2ef773 (python stdin heredoc).
//!
//! `REFUSED` is the exact command, recovered verbatim from the session; the
//! hook only judges it, nothing is executed.
//!
//! DOES NOT PROVE: which body line or chain position triggers it (not
//! isolated), the main-session verdict being correct, or anything about
//! other heredoc consumers.
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
    let dir = std::env::temp_dir().join(format!("bg-commit-hd-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

/// `(decision, reason)`. Exit status is checked first; only then is an empty
/// stdout read as Allow; a non-JSON stdout panics.
fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "commit-msg-heredoc-body-fp-20261118",
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

const MESSAGE: &str =
    "blastguard: ruling 9 correction — OtherWorktree rm -r probe becomes an Allow control (0.2.691)

User ruling 2026-11-17: recursive rm strictly inside a worktree storage root
stays Allow even when the session cwd is outside that checkout (scope.rs
2026-09-30 ruling takes precedence over ruling 9's \"confined Ask\"). The
ignored probe b1_probe_rm_r_own_worktree_target_with_cwd_outside is now the
un-ignored control b1_rm_r_other_worktree_with_cwd_outside_allows (main and
subagent Allow; observed allow/allow 2026-11-17).

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>";

const PREFIX: &str = "cd /Users/yuki/src/.harness-worktrees/ruling9-control-20261118 && . \"$HOME/.cargo/env\" && rustfmt --edition 2021 --check crates/blastguard/tests/ast_effect_rules_20261117.rs && git add crates/blastguard/tests/ast_effect_rules_20261117.rs crates/blastguard/Cargo.toml crates/blastguard/.claude-plugin/plugin.json .claude-plugin/marketplace.json Cargo.lock && ";

const SUFFIX: &str = "git log --oneline -1; git status --short | head -3";

/// The exact refused command.
fn refused() -> String {
    format!("{PREFIX}git commit -q -F - <<'EOF'\n{MESSAGE}\nEOF\n{SUFFIX}")
}

fn body_read_as_commands(r: &str) -> bool {
    r.contains("is not a command blastguard recognises")
}

/// RED: the message body of the quoted heredoc must not be judged as command
/// lines, in either mode. Only the body-as-command reason is asserted
/// against; another, independent verdict on the rest of the chain is out of
/// scope.
// test-weakening-justified: new RED for an observed FP (2026-11-18)
#[test]
fn refused_commit_heredoc_body_is_not_read_as_commands() {
    let home = unique_home("red");
    let command = refused();
    for subagent in [false, true] {
        let (d, r) = run_as(&command, &home, subagent);
        assert!(
            !(d != "allow" && body_read_as_commands(&r)),
            "subagent={subagent}: heredoc body judged as commands: {d} ({r})"
        );
    }
}

/// Control (minimal): the SAME quoted heredoc message as a bare first
/// `git commit -q -F -`, no chain before it.
#[test]
fn bare_commit_same_message_is_not_read_as_commands() {
    let home = unique_home("ctl");
    let command = format!("git commit -q -F - <<'EOF'\n{MESSAGE}\nEOF");
    for subagent in [false, true] {
        let (d, r) = run_as(&command, &home, subagent);
        assert!(
            !(d != "allow" && body_read_as_commands(&r)),
            "subagent={subagent}: heredoc body judged as commands: {d} ({r})"
        );
    }
}
