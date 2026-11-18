#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (false positive, observed 2026-11-18 in session 1e000416, relays
//! bgr-c1761eb0eaab and bgr-360d450f9569): a backtick-quoted `=word` inside a
//! QUOTED heredoc body (`<<'EOF'`) is reported as a command word when another
//! command follows the heredoc terminator:
//!   "the command word ``=x`` is an expansion whose value only exists at run
//!    time — blastguard cannot tell what program this runs"
//! A quoted heredoc body is never expanded, so nothing in it runs.
//!
//! Probe (worktree debug build of 0.2.691, main / subagent):
//!   cat <<'EOF'\n`=x`\nEOF\ntrue                       ask / deny  <- FP (minimal)
//!   cat <<'EOF'\n`=x`\nEOF                             allow / allow (no trailing command)
//!   cat > m.txt <<'EOF'\n`=rm -rf ~`\nEOF\ntrue          ask / deny  <- FP
//!   git commit -q -F - <<'EOF'\n`=rm -rf ~`\nEOF\ntrue   allow / allow (control)
//!   git commit ... <<'EOF'\n`=rm -rf ~`\nEOF\ngit log --oneline -1; backlog add --help 2>&1 | head -40
//!                                                       ask / deny  <- FP (the relayed shape)
//!   =$x foo                                             ask / deny  (genuine; control)
//!   bash <<EOF\nchmod 777 /etc/hosts\nEOF               deny / deny (genuine harm; control)
//!
//! Observed, deliberately NOT asserted: `cat > m.txt <<'EOF'\n`rm -rf ~`\nEOF\ntrue`
//! (backticked harm text, no `=`) is deny / deny in the worktree build.
//!
//! DOES NOT PROVE: why the git commit form escapes while the same body
//! followed by `git log ...; ... | head` does not; which fix is right.
//!
//! RED written by an independent test author, not the implementer.

#[path = "support/bounded.rs"]
mod bounded;

use std::process::{Command, Stdio};

const BT: char = '`';

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
        "bg-qheredoc-bt-equals-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "quoted-heredoc-backtick-equals-word-fp-20261118",
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

/// Every (command, subagent) pair whose decision is not `want(subagent)`.
fn mismatches(cmds: &[String], tag: &str, want: fn(bool) -> &'static str) -> Vec<String> {
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

/// `<head> <<'EOF'\n`<word>`\nEOF` followed by `tail` (if any).
fn quoted_heredoc(head: &str, word: &str, tail: &str) -> String {
    let mut s = format!("{head} <<'EOF'\n{BT}{word}{BT}\nEOF");
    if !tail.is_empty() {
        s.push('\n');
        s.push_str(tail);
    }
    s
}

#[test]
#[ignore = "RED: 2026-11-18 backtick `=word` in a quoted <<'EOF' body followed by another command is read as a command word (Ask/Deny) — remove the ignore when fixed"]
fn backtick_equals_word_in_quoted_heredoc_body_is_allow() {
    let bad = mismatches(
        &[
            quoted_heredoc("cat", "=x", "true"),
            quoted_heredoc("cat > m.txt", "=rm -rf ~", "true"),
            quoted_heredoc(
                "git commit -q -F -",
                "=rm -rf ~",
                "git log --oneline -1; backlog add --help 2>&1 | head -40",
            ),
        ],
        "red",
        |_| "allow",
    );
    assert!(
        bad.is_empty(),
        "a quoted heredoc body is never run; expected Allow:\n{}",
        bad.join("\n")
    );
}

#[test]
fn control_git_commit_quoted_heredoc_then_true_is_allow() {
    let bad = mismatches(
        &[quoted_heredoc("git commit -q -F -", "=rm -rf ~", "true")],
        "ctl-git",
        |_| "allow",
    );
    assert!(
        bad.is_empty(),
        "control must stay Allow:\n{}",
        bad.join("\n")
    );
}

#[test]
fn control_real_undetermined_equals_command_word_is_ask_main_deny_subagent() {
    let bad = mismatches(&["=$x foo".to_string()], "ctl-undet", |sub| {
        if sub {
            "deny"
        } else {
            "ask"
        }
    });
    assert!(
        bad.is_empty(),
        "a real `=$x` command word must stay restrictive:\n{}",
        bad.join("\n")
    );
}

#[test]
fn control_unquoted_heredoc_to_shell_running_harm_is_deny() {
    let bad = mismatches(
        &["bash <<EOF\nchmod 777 /etc/hosts\nEOF".to_string()],
        "ctl-harm",
        |_| "deny",
    );
    assert!(
        bad.is_empty(),
        "system harm fed to a shell via heredoc must stay Deny:\n{}",
        bad.join("\n")
    );
}
