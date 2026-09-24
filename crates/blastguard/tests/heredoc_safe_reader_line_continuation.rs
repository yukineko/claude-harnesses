// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! INDEPENDENT regression test for a fail-open introduced by c4682476.
//!
//! c4682476 (see `tests/heredoc_safe_reader_body.rs`) made
//! `inert_here_document_body` skip per-segment analysis of a here-document
//! BODY when the opener segment's `ends_at_newline` flag is set, the
//! delimiter is quoted, and the reader is a closed safe reader
//! (`git commit -F -` / `-F-` / `--file=-` / `--file -`, or a bare `cat`).
//!
//! `ends_at_newline` is computed in `split_segments_with_separators`
//! (`crates/blastguard/src/detect.rs`) purely from `c == '\n'` — it does not
//! look at whether that newline was preceded by a line-continuation
//! backslash. In real bash, a `\` immediately before a newline splices the
//! next physical line onto the CURRENT logical line, so
//!
//! ```text
//! git commit -F - <<'EOF' \
//! ; rm -rf ~/work
//! fix: thing
//! EOF
//! ```
//!
//! runs `; rm -rf ~/work` as a command on the opener's own logical line —
//! the here-document body only starts on the line after that. blastguard's
//! segmenter instead treats the bare `\n` right after `<<'EOF' \` as ending
//! the opener's line, so it marks `; rm -rf ~/work` (and `fix: thing`) as
//! the (inert) here-document BODY and never analyses `; rm -rf ~/work` as a
//! command at all. Base 564b3676 (full body analysis, no skip) denied all of
//! these; c4682476 allows them.
//!
//! Verified against real bash by an independent reviewer before this test
//! was written: the payload above executes `rm -rf ~/work`.
//!
//! Same harness conventions as `tests/heredoc_safe_reader_body.rs`: a
//! PreToolUse Bash JSON payload on stdin, `CLAUDECODE=1` +
//! `CLAUDE_CODE_ENTRYPOINT=cli` so an observed `Ask` is not hardened to
//! `Deny` before reaching stdout, and `CLAUDE_CODE_SESSION_ID` removed so the
//! repeat-refusal downgrade cannot fire.

use std::io::Write;
use std::process::{Command, Stdio};

/// Run the binary with a raw Bash `command` (may contain newlines/backticks/
/// `$`/backslashes) on stdin. Returns `permissionDecision` (`"allow"` when
/// the hook printed nothing) and the reason, if any.
fn verdict(command: &str) -> (String, String) {
    let bin = env!("CARGO_BIN_EXE_blastguard");
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": { "command": command },
    })
    .to_string();
    let mut child = Command::new(bin)
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
    assert_eq!(
        out.status.code(),
        Some(0),
        "hook must always exit 0 (cmd: {command:?})"
    );
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if stdout.trim().is_empty() {
        return ("allow".to_string(), String::new());
    }
    let doc: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout is not JSON for {command:?} ({e}): {stdout}"));
    let decision = doc
        .get("hookSpecificOutput")
        .and_then(|h| h.get("permissionDecision"))
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("no permissionDecision for {command:?}, got: {stdout}"))
        .to_string();
    let reason = doc
        .get("hookSpecificOutput")
        .and_then(|h| h.get("permissionDecisionReason"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    (decision, reason)
}

/// Asserts `command` is NOT allowed (i.e. is `ask` or `deny`), printing the
/// command and the observed decision/reason either way.
fn assert_not_allowed(property: &str, command: &str) {
    let (decision, reason) = verdict(command);
    assert!(
        decision != "allow",
        "{property}: expected ask/deny but got {decision:?} (reason: {reason:?}) for command:\n{command}"
    );
}

/// EXPECTED RED TODAY (backlog: line-continuation heredoc-opener fail-open).
/// `git commit -F - <<'EOF' \` followed by `; rm -rf ~/work` on the next
/// physical line is one logical opener line in real bash, so `rm -rf ~/work`
/// runs. blastguard's `ends_at_newline` does not account for the trailing
/// backslash, so it treats the body as starting one line early and marks
/// `; rm -rf ~/work` as inert here-document body, never analysing it.
#[test]
fn line_continuation_after_semicolon_git_dash_f_dash_is_not_allowed() {
    let cmd = "git commit -F - <<'EOF' \\\n; rm -rf ~/work\nfix: thing\nEOF";
    assert_not_allowed(
        "git commit -F - <<'EOF' \\ ; rm -rf ~/work (continuation, semicolon)",
        cmd,
    );
}

/// Same fail-open, `&&` instead of `;` after the continuation.
#[test]
fn line_continuation_after_and_and_git_dash_f_dash_is_not_allowed() {
    let cmd = "git commit -F - <<'EOF' \\\n&& rm -rf ~/work\nfix: thing\nEOF";
    assert_not_allowed(
        "git commit -F - <<'EOF' \\ && rm -rf ~/work (continuation, &&)",
        cmd,
    );
}

/// Same fail-open, `&` (background) instead of `;` after the continuation.
#[test]
fn line_continuation_after_ampersand_git_dash_f_dash_is_not_allowed() {
    let cmd = "git commit -F - <<'EOF' \\\n& rm -rf ~/work\nfix: thing\nEOF";
    assert_not_allowed(
        "git commit -F - <<'EOF' \\ & rm -rf ~/work (continuation, background &)",
        cmd,
    );
}

/// Same fail-open, `||` instead of `;` after the continuation.
#[test]
fn line_continuation_after_or_or_git_dash_f_dash_is_not_allowed() {
    let cmd = "git commit -F - <<'EOF' \\\n|| rm -rf ~/work\nfix: thing\nEOF";
    assert_not_allowed(
        "git commit -F - <<'EOF' \\ || rm -rf ~/work (continuation, ||)",
        cmd,
    );
}

/// Doubled continuation (`\` then a line that is only `\`, then the
/// destructive line) must not launder the payload either.
#[test]
fn doubled_line_continuation_git_dash_f_dash_is_not_allowed() {
    let cmd = "git commit -F - <<'EOF' \\\n\\\n; rm -rf ~/work\nfix\nEOF";
    assert_not_allowed(
        "git commit -F - <<'EOF' \\ \\ ; rm -rf ~/work (doubled continuation)",
        cmd,
    );
}

/// Same fail-open, `--file=-` spelling of the safe-reader condition.
#[test]
fn line_continuation_git_file_eq_dash_is_not_allowed() {
    let cmd = "git commit --file=- <<'EOF' \\\n; rm -rf ~/work\nfix\nEOF";
    assert_not_allowed(
        "git commit --file=- <<'EOF' \\ ; rm -rf ~/work (continuation)",
        cmd,
    );
}

/// Same fail-open, `--file -` spelling of the safe-reader condition.
#[test]
fn line_continuation_git_file_space_dash_is_not_allowed() {
    let cmd = "git commit --file - <<'EOF' \\\n; rm -rf ~/work\nfix\nEOF";
    assert_not_allowed(
        "git commit --file - <<'EOF' \\ ; rm -rf ~/work (continuation)",
        cmd,
    );
}

/// Same fail-open, `-F-` (no space) spelling of the safe-reader condition.
#[test]
fn line_continuation_git_dash_f_no_space_is_not_allowed() {
    let cmd = "git commit -F- <<'EOF' \\\n; rm -rf ~/work\nfix\nEOF";
    assert_not_allowed(
        "git commit -F- <<'EOF' \\ ; rm -rf ~/work (continuation)",
        cmd,
    );
}

/// Control: the same continuation shape with a bare `cat` reader. The
/// independent verifier reported this one is already caught today — kept
/// here so a regression in the `cat` arm shows up next to the `git` arms.
#[test]
fn line_continuation_bare_cat_is_not_allowed_control() {
    let cmd = "cat <<'EOF' \\\n; rm -rf ~/work\nx\nEOF";
    assert_not_allowed(
        "cat <<'EOF' \\ ; rm -rf ~/work (continuation, control)",
        cmd,
    );
}

/// Same fail-open, but the payload reached via continuation is an
/// unresolvable command-word expansion (`$CMD x`) rather than a literal
/// destructive command — the class of command this scan is supposed to
/// `ask` on when it cannot resolve the command word.
#[test]
fn line_continuation_unresolvable_command_word_is_not_allowed() {
    let cmd = "git commit -F - <<'EOF' \\\n; $CMD x\nfix\nEOF";
    assert_not_allowed(
        "git commit -F - <<'EOF' \\ ; $CMD x (continuation, unresolvable command word)",
        cmd,
    );
}

/// Positive control: a quoted-delimiter `git commit -F -` heredoc with NO
/// line continuation on the opener must still be allowed — this test must
/// NOT undo the `c4682476` fix for `tests/heredoc_safe_reader_body.rs`.
#[test]
fn no_continuation_quoted_delimiter_safe_reader_still_allowed() {
    let cmd = "git commit -F - <<'EOF'\n`done` is set\nEOF";
    let (decision, reason) = verdict(cmd);
    assert_eq!(
        decision, "allow",
        "positive control regressed: expected allow but got {decision:?} (reason: {reason:?}) for command:\n{cmd}"
    );
}
