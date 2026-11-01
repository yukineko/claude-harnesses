// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Adversarial pins for the widened allow surface of `opener_consumer`
//! (`cd <literal> &&` / `git add <literal>... &&` heads in front of a quoted
//! here-doc opener) and of `permitted_prefix_line` (`git add <literal>...`).
//!
//! The quoted body is data, so a `>` or a backtick in it is not a redirect.
//! Everything else on the command must still be judged: a head that is not a
//! plain `cd`/`git add` with literal arguments, a redirect on the opener line,
//! a redirect after the closing delimiter, and an unquoted delimiter must all
//! stay restrictive (deny or ask). Each case runs with a fresh HOME so the
//! repeat-deny downgrade cannot change a verdict.

use std::io::Write;
use std::process::{Command, Stdio};

fn verdict(tag: &str, command: &str) -> (String, String) {
    let home = tempfile::TempDir::new().expect("mkdir home");
    let bin = env!("CARGO_BIN_EXE_blastguard");
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": { "command": command },
    })
    .to_string();
    let mut child = Command::new(bin)
        .env("HOME", home.path())
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
        eprintln!("[{tag}] allow");
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
    eprintln!("[{tag}] {decision}: {reason}");
    (decision, reason)
}

const BODY: &str = "run from > `.`";

fn heredoc(opener_head: &str) -> String {
    format!("{opener_head} git commit -F - <<'EOF'\n{BODY}\nEOF")
}

fn assert_restrictive(tag: &str, cmd: &str) {
    let (d, r) = verdict(tag, cmd);
    assert!(
        d == "deny" || d == "ask",
        "[{tag}] expected deny/ask, got {d} ({r}) for {cmd:?}"
    );
}

#[test]
fn newline_separated_git_add_line_then_quoted_heredoc_is_allowed() {
    let cmd = format!("git add a.txt\ngit commit -F - <<'EOF'\n{BODY}\nEOF");
    let (d, r) = verdict("nl-twin", &cmd);
    assert_eq!(d, "allow", "reason: {r}");
}

#[test]
fn cd_then_git_add_then_commit_chain_is_allowed() {
    let (d, r) = verdict("cd-add-chain", &heredoc("cd /tmp && git add a.txt &&"));
    assert_eq!(d, "allow", "reason: {r}");
}

#[test]
fn head_with_real_backtick_redirect_stays_restrictive() {
    assert_restrictive("add-redirect-head", &heredoc("git add a.txt > `pwd` &&"));
}

#[test]
fn head_with_variable_path_stays_restrictive() {
    assert_restrictive("add-var", &heredoc("git add $X &&"));
}

#[test]
fn head_with_git_global_option_stays_restrictive() {
    assert_restrictive("git-c-add", &heredoc("git -c a=b add a &&"));
}

#[test]
fn head_git_rm_stays_restrictive() {
    assert_restrictive("git-rm", &heredoc("git rm -rf x &&"));
}

#[test]
fn head_rm_rf_stays_restrictive() {
    assert_restrictive("rm-rf", &heredoc("rm -rf build &&"));
}

#[test]
fn head_or_stays_restrictive() {
    assert_restrictive("or-head", &heredoc("git add a.txt ||"));
}

#[test]
fn head_semicolon_stays_restrictive() {
    assert_restrictive("semi-head", &heredoc("git add a.txt ;"));
}

#[test]
fn head_pipe_stays_restrictive() {
    assert_restrictive("pipe-head", &heredoc("git add a.txt |"));
}

#[test]
fn real_redirect_after_closing_delimiter_stays_restrictive() {
    let cmd = format!("git add a.txt && git commit -F - <<'EOF'\n{BODY}\nEOF\necho hi > `pwd`");
    assert_restrictive("after-close", &cmd);
}

#[test]
fn unquoted_delimiter_stays_restrictive() {
    let cmd = format!("git add a.txt && git commit -F - <<EOF\n{BODY}\nEOF");
    assert_restrictive("unquoted", &cmd);
}

#[test]
fn destructive_middle_head_stays_restrictive() {
    assert_restrictive(
        "destructive-middle",
        &heredoc("git add a.txt && rm -rf ~ &&"),
    );
}

#[test]
fn real_redirect_on_opener_line_stays_restrictive() {
    let cmd = format!("git add a.txt && git commit -F - <<'EOF' > out.txt\n{BODY}\nEOF");
    assert_restrictive("opener-redirect", &cmd);
}

/// Probe (report-only): what `git add -- a.txt` and `git add .` heads return.
/// Asserts only a well-formed answer; the values are printed with --nocapture
/// and reported by the verifier.
#[test]
fn probe_git_add_dashdash_and_dot_report_verdict() {
    for (tag, head) in [
        ("add-dashdash", "git add -- a.txt &&"),
        ("add-dot", "git add . &&"),
    ] {
        let (d, _) = verdict(tag, &heredoc(head));
        assert!(
            d == "allow" || d == "deny" || d == "ask",
            "[{tag}] unexpected decision {d}"
        );
    }
}
