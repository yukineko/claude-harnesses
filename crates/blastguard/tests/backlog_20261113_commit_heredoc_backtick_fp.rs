// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro (observed 2026-11-13 in a subagent session, measured at main
//! 1ac49fbf): a `git commit -F - <<'EOF'` whose QUOTED-delimiter heredoc body
//! contains a backtick span such as `` `blastguard log` `` is denied with the
//! verbatim reason
//!
//!   the command word ``blastguard` is an expansion whose value only exists at
//!   run time — blastguard cannot tell what program this runs
//!
//! The shell performs no expansion inside a quoted-delimiter heredoc body
//! (POSIX: "If any part of word is quoted ... the here-document lines shall not
//! be expanded"), so the backticks are data passed to git on stdin; nothing in
//! the body is executed. In a subagent an Ask is turned into a deny, so this
//! blocks a worker's commit outright.
//!
//! The command below is the observed shape (explicit-path `git add`, then
//! `git commit -q -F - <<'EOF'`, a body with backtick spans, then trailing
//! commands); paths and the body text are shortened but keep the same tokens.
//!
//! CONTROLS: an UNQUOTED heredoc DOES expand backtick substitutions before the
//! command runs, so a backtick span that deletes `$HOME` must still not be
//! allowed silently — both fed to a shell and as a `git commit` message.
//!
//! DOES NOT PROVE: which internal stage misreads the body (heredoc splitting vs
//! the command-word expansion check), nor anything about `$(...)` in a quoted
//! body, nor the `--json`/log side.

#[path = "support/bounded.rs"]
mod bounded;

use std::process::{Command, Stdio};

fn verdict(command: &str) -> (String, String) {
    let bin = env!("CARGO_BIN_EXE_blastguard");
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": { "command": command },
    })
    .to_string();
    let mut hook = Command::new(bin);
    hook.env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = bounded::run_bounded(&mut hook, payload.as_bytes(), bounded::HOOK_DEADLINE);
    assert_eq!(out.status.code(), Some(0), "hook must exit 0 ({command:?})");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if stdout.trim().is_empty() {
        return ("allow".to_string(), String::new());
    }
    let doc: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout not JSON for {command:?} ({e}): {stdout}"));
    let hso = doc.get("hookSpecificOutput");
    let d = hso
        .and_then(|h| h.get("permissionDecision"))
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("no permissionDecision for {command:?}: {stdout}"))
        .to_string();
    let r = hso
        .and_then(|h| h.get("permissionDecisionReason"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    (d, r)
}

const OBSERVED: &str = "cd /tmp/wt-o3 && git add crates/blastguard/src/log.rs docs/specs/blastguard.md && git commit -q -F - <<'EOF'\n\
fix(blastguard): escape C1/U+2028/U+2029/bidi controls in `log` text; bump 0.2.474\n\
\n\
`blastguard log` text mode printed C1 controls unescaped\n\
(backlog 81cd7f45, audit O3). escape_text now writes them as `\\u{XXXX}`.\n\
\n\
Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>\n\
EOF\n\
echo \"o3 commit=$?\"; git log --oneline -1; git status --short";

#[test]
#[ignore = "RED: commit heredoc backtick FP (2026-11-13) — remove the ignore when fixed"]
fn quoted_heredoc_commit_message_with_backticks_is_not_denied_or_asked() {
    let (d, r) = verdict(OBSERVED);
    assert!(
        d != "deny" && d != "ask",
        "a quoted-delimiter heredoc body is data (no expansion); got {d}: {r}"
    );
}

#[test]
fn control_unquoted_heredoc_backtick_fed_to_shell_is_not_allowed_silently() {
    let cmd = "sh <<EOF\necho `rm -rf \"$HOME\"`\nEOF";
    let (d, r) = verdict(cmd);
    assert_ne!(
        d, "allow",
        "an unquoted heredoc expands the backtick substitution; must not be allowed: {r}"
    );
}

#[test]
fn control_unquoted_heredoc_backtick_in_commit_message_is_not_allowed_silently() {
    let cmd = "git commit -q -F - <<EOF\nfix: `rm -rf \"$HOME\"`\nEOF";
    let (d, r) = verdict(cmd);
    assert_ne!(
        d, "allow",
        "an unquoted heredoc expands the backtick substitution before git runs: {r}"
    );
}
