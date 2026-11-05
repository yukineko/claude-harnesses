// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro (scout 2026-11-05): quoted-delimiter heredoc BODIES misread as
//! shell syntax. Observed in a worker session on 2026-11-05; the commands below
//! are RECONSTRUCTED (the original text was not preserved), the deny/ask
//! reasons are the verbatim ones from that session.
//!
//! (a) `git commit -F- <<'MSG' ... MSG` whose body ends a trailer line with `>`
//!     (`Co-Authored-By: A <a@b>`): the `>` followed by the delimiter line `MSG`
//!     is read as the relative redirect `> MSG`. The shell does no parsing of a
//!     quoted-delimiter body, so this is data. In a subagent Ask becomes deny,
//!     so an Ask here blocks a worker's commit.
//! (b) a python stdin heredoc whose only mention of `os.unlink(` is inside a
//!     string literal that is never exec'd/eval'd. blastguard denies it as
//!     "a quoted string ... spells `unlink(`". Two readings: INTENDED (the
//!     string might be fed to exec; blastguard does not parse python) vs FALSE
//!     POSITIVE (a docstring/message). Tell apart: a string that reaches
//!     exec/eval/compile must stay denied (control below); a bare print must not.
//! (c) `cd "$WT" && cat <<'EOF' ... > {x} ... EOF`: body `> {x}` read as a
//!     RELATIVE redirect after an unresolvable cd. `cat` to stdout writes nothing.

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

#[test]
#[ignore = "backlog: open defect (scout 2026-11-05) quoted heredoc body trailer `>` read as `> MSG` redirect"]
fn a_git_commit_heredoc_trailer_gt_before_delimiter_is_not_a_redirect() {
    let cmd = "cd /tmp/wt; git commit -F- <<'MSG'\nfix(x): y\n\nCo-Authored-By: A <a@b>\nMSG";
    let (d, r) = verdict(cmd);
    assert_eq!(d, "allow", "quoted heredoc body is data; got {d}: {r}");
}

#[test]
#[ignore = "backlog: open defect (scout 2026-11-05) python string literal mentioning os.unlink( denied"]
fn b_python_string_literal_mentioning_unlink_is_not_denied() {
    let cmd = "python3 - <<'PY'\nx = \"docs: os.unlink( removes a file\"\nprint(x)\nPY";
    let (d, r) = verdict(cmd);
    assert_ne!(d, "deny", "a never-executed string must at most Ask: {r}");
}

#[test]
fn b_control_string_fed_to_exec_stays_denied() {
    let cmd = "python3 - <<'PY'\nexec(\"import os; os.unlink('/tmp/x')\")\nPY";
    let (d, r) = verdict(cmd);
    assert_ne!(
        d, "allow",
        "exec of a deleting string must not be allowed: {r}"
    );
}

#[test]
#[ignore = "backlog: open defect (scout 2026-11-05) quoted heredoc body `> {x}` read as relative redirect after cd"]
fn c_cat_quoted_heredoc_body_gt_brace_after_unresolved_cd_is_not_a_redirect() {
    let cmd = "cd \"$WT\" && cat <<'EOF'\nthe value > {bar} holds\nEOF";
    let (d, r) = verdict(cmd);
    assert_eq!(d, "allow", "quoted heredoc body is data; got {d}: {r}");
}

#[test]
fn c_control_real_relative_redirect_after_unresolved_cd_still_blocked() {
    let (d, r) = verdict("cd \"$WT\" && echo hi > {bar}");
    assert_ne!(d, "allow", "real redirect must not be allowed: {r}");
}
