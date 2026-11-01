// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `git add <paths> && git commit -F - <<'EOF' ... EOF` where the commit
//! message body contains backticks and "> `.`" was blocked live with:
//!
//! ```text
//! '> `.' destroys the file's current contents and ``.` contains a shell
//! expansion, so blastguard cannot tell which file this names, let alone
//! whether its contents are recoverable
//! ```
//!
//! With a quoted delimiter the body is literal stdin to `git commit`; neither
//! the `>` nor the backticks are a redirect or an expansion. The sibling tests
//! (`heredoc_quoted_body_redirect_false_positive.rs`) cover `cat`/`python3`
//! readers; this one pins the `git commit -F -` reader behind an `&&` chain.
//!
//! Each case runs with a fresh HOME so the repeat-deny downgrade cannot
//! change a verdict.

use std::io::Write;
use std::process::{Command, Stdio};

fn verdict(tag: &str, command: &str) -> (String, String) {
    let home = std::env::temp_dir().join(format!(
        "blastguard-heredoc-bt-{}-{tag}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).expect("mkdir home");
    let bin = env!("CARGO_BIN_EXE_blastguard");
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": { "command": command },
    })
    .to_string();
    let mut child = Command::new(bin)
        .env("HOME", &home)
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
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
    assert_eq!(out.status.code(), Some(0), "hook must exit 0 ({command:?})");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if stdout.trim().is_empty() {
        return ("allow".to_string(), String::new());
    }
    let doc: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout not JSON for {command:?} ({e}): {stdout}"));
    let hso = doc.get("hookSpecificOutput");
    let decision = hso
        .and_then(|h| h.get("permissionDecision"))
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("no permissionDecision for {command:?}: {stdout}"))
        .to_string();
    let reason = hso
        .and_then(|h| h.get("permissionDecisionReason"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    (decision, reason)
}

const BODY: &str = "fix: use `tempfile::Builder` for the scratch dir\n\nrun from > `.` and keep `a` > `b` as prose\n\nCo-Authored-By: X <x@example.com>";

/// (a) The reported shape. Control `git commit -F file` is the baseline; the
/// heredoc variants must get the same verdict (no redirect/expansion reason).
#[test]
fn quoted_heredoc_git_commit_f_stdin_body_is_data() {
    let (base_d, base_r) = verdict("base", "git add a.txt && git commit -F msg.txt");
    let mut bad = Vec::new();
    for (tag, cmd) in [
        (
            "chain",
            format!("git add a.txt b.txt && git commit -F - <<'EOF'\n{BODY}\nEOF"),
        ),
        ("plain", format!("git commit -F - <<'EOF'\n{BODY}\nEOF")),
        (
            "minimal",
            "git commit -F - <<'EOF'\nrun from > `.`\nEOF".to_string(),
        ),
    ] {
        let (d, r) = verdict(tag, &cmd);
        if d != base_d {
            bad.push(format!(
                "[{tag}] got {d} (reason: {r}); control `git commit -F file` got {base_d} ({base_r})"
            ));
        }
    }
    assert!(
        bad.is_empty(),
        "quoted heredoc body judged as redirect/expansion:\n{}",
        bad.join("\n")
    );
}

/// CONTROL: a real redirect whose target is a backtick expansion, outside any
/// heredoc, stays blocked.
#[test]
fn control_real_backtick_redirect_outside_heredoc_stays_blocked() {
    let (d, r) = verdict("real", "echo hi > `pwd`");
    assert!(d == "deny" || d == "ask", "got {d} ({r})");
}

/// CONTROL (pinned today's behaviour): an UNQUOTED heredoc body really
/// undergoes command substitution, so it must stay blocking.
#[test]
fn control_unquoted_heredoc_backtick_body_stays_blocked() {
    let (d, r) = verdict(
        "unq",
        "git add a.txt && git commit -F - <<EOF\nrun from > `.`\nEOF",
    );
    assert!(d == "deny" || d == "ask", "got {d} ({r})");
}
