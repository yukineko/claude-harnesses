// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Adversarial pins for the widened allow surface of `opener_consumer`
//! (`cd <literal> &&` / `git add <literal>... &&` heads in front of a quoted
//! here-doc opener) and of `permitted_prefix_line` (`git add <literal>...`).
//!
//! The quoted body is data, so a `>` or a backtick in it is not a redirect.
//! Everything else on the command must still be judged: a redirect on the
//! opener line, a redirect after the closing delimiter, a real backtick
//! redirect in the head, a destructive head, and an unquoted delimiter must
//! all stay restrictive (deny or ask).
//!
//! User ruling 2026-11-18 (B-2): a head in front of the data here-document is
//! judged on its own merits, not by whether it is one of a whitelisted shape.
//! So the `parity_*` cases require the combined command to get exactly the
//! strictest of (the bare here-document alone, each head command alone): no
//! more permissive than any part, and no stricter just because the parts are
//! joined (`||`, `;`, `|`, `git -c`, `git rm`, `rm -rf build`, `git add $X`).
//! A head that is restrictive alone (for example `rm -rf build`, deny alone
//! at origin/main 04d9b8336) therefore keeps that verdict on the combined
//! line; `git rm -rf x` alone was observed Allow there, so its parity is Allow. Each case runs with a fresh HOME
//! so the repeat-deny downgrade cannot change a verdict.

#[path = "support/bounded.rs"]
mod bounded;

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
    let mut hook = Command::new(bin);
    hook.env("HOME", home.path())
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = bounded::run_bounded(&mut hook, payload.as_bytes(), bounded::HOOK_DEADLINE);
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

fn rank(tag: &str, d: &str) -> u8 {
    match d {
        "allow" => 0,
        "ask" => 1,
        "deny" => 2,
        other => panic!("[{tag}] unexpected decision {other:?}"),
    }
}

/// The combined command must get exactly the strictest verdict among the
/// bare here-document alone and each head command alone (ruling B-2).
fn assert_parity(tag: &str, head_parts: &[&str], combined: &str) {
    let bare = format!("git commit -F - <<'EOF'\n{BODY}\nEOF");
    let (mut want, _) = verdict(&format!("{tag}/bare"), &bare);
    for part in head_parts {
        let (d, _) = verdict(&format!("{tag}/part {part:?}"), part);
        if rank(tag, &d) > rank(tag, &want) {
            want = d;
        }
    }
    let (got, r) = verdict(tag, combined);
    assert_eq!(
        got, want,
        "[{tag}] combined must equal the strictest of bare heredoc + head parts \
         ({head_parts:?}); got {got} ({r}) for {combined:?}"
    );
}

#[test]
fn parity_head_with_variable_path() {
    assert_parity("add-var", &["git add $X"], &heredoc("git add $X &&"));
}

#[test]
fn parity_head_with_git_global_option() {
    assert_parity(
        "git-c-add",
        &["git -c a=b add a"],
        &heredoc("git -c a=b add a &&"),
    );
}

#[test]
fn parity_head_git_rm() {
    assert_parity("git-rm", &["git rm -rf x"], &heredoc("git rm -rf x &&"));
}

/// Already holds at origin/main 04d9b8336: `rm -rf build` alone is deny, so
/// the combined line keeps deny.
#[test]
fn parity_head_rm_rf() {
    assert_parity("rm-rf", &["rm -rf build"], &heredoc("rm -rf build &&"));
}

#[test]
fn parity_head_or() {
    assert_parity("or-head", &["git add a.txt"], &heredoc("git add a.txt ||"));
}

#[test]
fn parity_head_semicolon() {
    assert_parity("semi-head", &["git add a.txt"], &heredoc("git add a.txt ;"));
}

#[test]
fn parity_head_pipe() {
    assert_parity("pipe-head", &["git add a.txt"], &heredoc("git add a.txt |"));
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
