#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (false-positive candidate, observed 2026-11-17 in session 1e000416,
//! relay bgr-ce850d320ab9): a subagent's
//! `cd <wt> && backlog add ... --notes "$(cat <<'EOF'\n...`x`...\nEOF\n)" 2>&1 | tail -20`
//! was denied with (verbatim):
//!   "a backtick inside a double-quoted argument of `backlog add` is command
//!    substitution: the shell runs that span and silently deletes it from the
//!    stored text. Use single quotes (or escape the backtick) to keep it literal"
//! The same command re-run in the main session stored the backticks literally
//! (backlog row dc97fc2d), as bash never expands a QUOTED heredoc body, and the
//! output of `$(...)` is not re-scanned for substitutions.
//!
//! Probe (installed 0.2.679 and worktree 0.2.684 agree; main / subagent):
//!   Q1 echo "$(cat <<'EOF'\n`ls`\nEOF\n)"                       ask / deny  <- FP ("the command executed here comes from `ls` ...")
//!   Q2 backlog add --title t --notes "$(cat <<'EOF'\n`x`...)"    ask / deny  <- FP (backtick_text.rs reason above)
//!   U2 echo "$(cat <<EOF\n`ls`\nEOF\n)"                         ask / deny  (genuine substitution; control)
//!   H1 backlog add ... "$(cat <<EOF\n`chmod 777 /etc/hosts`...)" deny / deny (genuine system harm; control)
//!   H2 echo "$(cat <<EOF\n`chmod 777 /etc/hosts`\nEOF\n)"       deny / deny (control)
//!   D1 backlog add --title t --notes "see `ls`"                 ask / deny  (genuine substitution; control)
//!   H3 backlog add ... "$(cat <<'EOF'\n`chmod 777 /etc/hosts`...)" ask / deny (observed, deliberately NOT asserted)
//!
//! Two detector paths are involved: Q2 is `detect/backtick_text.rs`
//! (`backtick_in_double_quoted_text`, a quote-only char scan that does not
//! model `$(...)` / quoted heredoc bodies); Q1 is a separate detector
//! ("the command executed here comes from ...") that is not limited to
//! `backlog` / `git commit`. Related: backlog 234cae6f (same shape for
//! `git commit -m`, whose RED does not cover `backlog add` or `echo`).
//!
//! DOES NOT PROVE: which fix is right, the H3 verdict (a quoted body holding a
//! system-harm string), `backlog edit`, or other wrapper commands than `cat`.
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
        "bg-qheredoc-dq-cmdsubst-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "quoted-heredoc-backtick-dq-cmdsubst-20261117",
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

/// Every (command, subagent) pair whose decision is not in `want(subagent)`.
fn mismatches(cmds: &[&str], tag: &str, want: fn(bool) -> &'static str) -> Vec<String> {
    let home = unique_home(tag);
    let mut bad = Vec::new();
    for cmd in cmds {
        for subagent in [false, true] {
            let (d, r) = run_as(cmd, &home, subagent);
            if d != want(subagent) {
                bad.push(format!(
                    "subagent={subagent} {cmd:?} => {d} (want {}) ({r})",
                    want(subagent)
                ));
            }
        }
    }
    bad
}

const Q1_ECHO_QUOTED: &str = "echo \"$(cat <<'EOF'\n`ls`\nEOF\n)\"";
const Q2_BACKLOG_QUOTED: &str =
    "backlog add --title t --notes \"$(cat <<'EOF'\nsee `.backlog/tasks.toml`\nEOF\n)\"";
const Q3_BACKLOG_QUOTED_CHAIN: &str = "cd /tmp && backlog add --title t --notes \"$(cat <<'EOF'\nsee `git status --short`\nEOF\n)\" 2>&1 | tail -20";

#[test]
#[ignore = "RED: 2026-11-17 backtick in a quoted <<'EOF' body inside \"$(cat ...)\" is Ask/Deny (echo and backlog add) — remove the ignore when fixed"]
fn backtick_in_quoted_heredoc_inside_dq_cmdsubst_is_allow() {
    let bad = mismatches(
        &[Q1_ECHO_QUOTED, Q2_BACKLOG_QUOTED, Q3_BACKLOG_QUOTED_CHAIN],
        "red",
        |_| "allow",
    );
    assert!(
        bad.is_empty(),
        "expected Allow for every quoted-heredoc shape:\n{}",
        bad.join("\n")
    );
}

#[test]
fn control_quoted_heredoc_without_backtick_is_allow() {
    let bad = mismatches(
        &["backlog add --title t --notes \"$(cat <<'EOF'\nsee .backlog/tasks.toml\nEOF\n)\""],
        "ctl-plain",
        |_| "allow",
    );
    assert!(
        bad.is_empty(),
        "control must stay Allow:\n{}",
        bad.join("\n")
    );
}

#[test]
fn control_genuine_substitution_stays_ask_in_main_deny_in_subagent() {
    // U2: unquoted heredoc body backticks ARE expanded by bash.
    // D1: backtick written directly inside a double-quoted argument.
    let bad = mismatches(
        &[
            "echo \"$(cat <<EOF\n`ls`\nEOF\n)\"",
            "backlog add --title t --notes \"see `ls`\"",
        ],
        "ctl-subst",
        |sub| if sub { "deny" } else { "ask" },
    );
    assert!(
        bad.is_empty(),
        "genuine substitution must stay restrictive:\n{}",
        bad.join("\n")
    );
}

#[test]
fn control_unquoted_heredoc_system_harm_stays_deny() {
    let bad = mismatches(
        &[
            "backlog add --title t --notes \"$(cat <<EOF\n`chmod 777 /etc/hosts`\nEOF\n)\"",
            "echo \"$(cat <<EOF\n`chmod 777 /etc/hosts`\nEOF\n)\"",
        ],
        "ctl-harm",
        |_| "deny",
    );
    assert!(
        bad.is_empty(),
        "system harm via an unquoted heredoc must stay Deny:\n{}",
        bad.join("\n")
    );
}
