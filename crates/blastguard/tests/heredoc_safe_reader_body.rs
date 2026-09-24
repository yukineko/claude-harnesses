// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Regression tests for item 6cf12ce9.
//!
//! Spec (operator ruling): blastguard currently analyses every here-document
//! BODY line as if it were a command, so a quoted-delimiter commit message
//! whose line starts with a markdown span (`` `done` is set ``) is read as an
//! unresolvable command-word expansion and produces an `Ask` — even though a
//! quoted delimiter (`<<'EOF'`, `<<"EOF"`, `<<\EOF`, `<<-'EOF'`) tells the
//! shell to substitute NOTHING in the body, so the body is inert data, not
//! code.
//!
//! The fix (not yet made — these tests are written against the CURRENT,
//! unfixed binary) is to skip body analysis ONLY when BOTH hold:
//!   (a) the delimiter is quoted;
//!   (b) the program reading the body is a closed safe reader:
//!       - `git commit -F -` / `-F-` / `--file=-` / `--file -`, optionally
//!         with git global options (`-C <dir>`, `-q`, …);
//!       - a bare `cat` with no file operand and no output redirect/pipe.
//! AND nothing follows the heredoc operator on the opener's own line, AND the
//! here-document is closed by an exact delimiter line. Lines after the close
//! are ordinary code and stay analysed. Every other shape (other reader,
//! unquoted delimiter, safe reader piped/redirected, unclosed heredoc) keeps
//! full body analysis.
//!
//! These are end-to-end tests against the real built binary, following the
//! pattern in `tests/integration.rs` / `tests/repeat_downgrade.rs`:
//!   * a PreToolUse Bash JSON payload on stdin (built with `serde_json` so
//!     backticks/`$`/newlines in the command text are encoded correctly,
//!     rather than hand-escaped into a Rust string literal);
//!   * `CLAUDECODE=1` + `CLAUDE_CODE_ENTRYPOINT=cli` so an `Ask` this file
//!     means to observe is not hardened into a `Deny` by
//!     `crate::interactive`/`Decision::hardened` before it reaches stdout;
//!   * `CLAUDE_CODE_SESSION_ID` removed, so the repeat-refusal downgrade
//!     (`tests/repeat_downgrade.rs`) cannot fire — an unattributable run
//!     resolves to `Undetermined` = no downgrade, so every command here is
//!     judged at full strength regardless of what any other command in this
//!     file (or a previous `cargo test` run) produced.

use std::io::Write;
use std::process::{Command, Stdio};

/// Run the binary with a raw Bash `command` (may contain newlines/backticks/
/// `$`) on stdin. Returns `permissionDecision` (`"allow"` when the hook
/// printed nothing) and the reason, if any.
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

/// EXPECTED RED TODAY. A quoted-delimiter heredoc body fed to a closed safe
/// reader (`git commit -F -`/`--file=-`/`--file -`, or bare `cat`) must not be
/// analysed as code: the shell substitutes nothing into a quoted-delimiter
/// body, so a line that merely LOOKS like a command word (a backtick span, a
/// `$VAR`) is inert text, not an expansion. Today every one of these is
/// wrongly `ask`ed on the backtick/`$` body line.
#[test]
fn quoted_delimiter_safe_reader_body_is_not_analysed_as_code() {
    let cases: [(&str, &str); 7] = [
        (
            "git commit -F - with -q and a leading markdown span in the body",
            "git commit -q -F - <<'EOF'\nfix: thing\n\n`done` is set\nEOF",
        ),
        (
            "git -C <dir> commit -F -",
            "git -C /tmp/somewhere commit -F - <<'MSG'\n`backlog session-start` now claims\nMSG",
        ),
        (
            "git commit --file=- with a double-quoted delimiter",
            "git commit --file=- <<\"EOF\"\n`x` y\nEOF",
        ),
        (
            "git commit -F - with a backslash-escaped delimiter",
            "git commit -F - <<\\EOF\n`x` y\nEOF",
        ),
        (
            "git commit -F - with <<-'EOF' (tab-stripping, quoted)",
            "git commit -F - <<-'EOF'\n\t`done`\n\tEOF",
        ),
        (
            "bare cat with a quoted delimiter",
            "cat <<'EOF'\n`done` and $HOME\nEOF",
        ),
        (
            "git commit -F - body containing a bare $VAR",
            "git commit -F - <<'EOF'\n$CMD is mentioned\nEOF",
        ),
    ];
    let mut not_allowed: Vec<(&str, &str, String, String)> = Vec::new();
    for (label, cmd) in cases {
        let (decision, reason) = verdict(cmd);
        if decision != "allow" {
            not_allowed.push((label, cmd, decision, reason));
        }
    }
    assert!(
        not_allowed.is_empty(),
        "a closed safe reader with a quoted delimiter must not analyse the body \
as code — these cases are still ask/deny today: {not_allowed:#?}"
    );
}

/// Negative controls: shapes that must NOT be allowed, because at least one of
/// the safe-reader conditions fails. Per spec these should already hold on
/// today's (unfixed) code, since body-skipping does not exist yet — every one
/// of these already gets full body analysis, and the ones with a genuinely
/// destructive/unresolvable body line are already `ask`/`deny`. Any case that
/// is NOT ask/deny today is flagged as a pre-existing gap, not papered over.
#[test]
fn negative_controls_stay_ask_or_deny() {
    let cases: [(&str, &str); 12] = [
        (
            "unrecognised reader (bash) even with a quoted delimiter",
            "bash <<'EOF'\nrm -rf ~/work\nEOF",
        ),
        (
            "unrecognised reader (sh) with a quoted delimiter and $VAR body",
            "sh <<'EOF'\n$CMD x\nEOF",
        ),
        (
            "cat piped into sh — not a closed safe reader",
            "cat <<'EOF' | sh\n$CMD x\nEOF",
        ),
        (
            "cat piped into sh with a destructive body",
            "cat <<'EOF' | sh\nrm -rf ~/work\nEOF",
        ),
        (
            "cat with an UNQUOTED delimiter (command substitution runs)",
            "cat <<EOF\n$(rm -rf ~/work)\nEOF",
        ),
        (
            "git commit -F - with an UNQUOTED delimiter",
            "git commit -F - <<EOF\n`done` is set\nEOF",
        ),
        (
            "reader not on the safe list (python3)",
            "python3 - <<'PY'\n`x`\nPY",
        ),
        (
            "code after `;` on the opener's own line",
            "cat <<'EOF'; $CMD x\n`done`\nEOF",
        ),
        (
            "code after `&&` on the opener's own line",
            "git commit -F - <<'EOF' && $CMD x\n`done`\nEOF",
        ),
        (
            "unclosed heredoc",
            "git commit -F - <<'EOF'\n`done` never closed",
        ),
        (
            "code after the close is still judged",
            "git commit -F - <<'EOF'\nmsg\nEOF\n$CMD x",
        ),
        (
            "cat with an output redirect is not a safe reader",
            "cat > run.sh <<'EOF'\n$CMD x\nEOF",
        ),
    ];
    let mut not_gated: Vec<(&str, &str, String, String)> = Vec::new();
    for (label, cmd) in cases {
        let (decision, reason) = verdict(cmd);
        if decision != "ask" && decision != "deny" {
            not_gated.push((label, cmd, decision, reason));
        }
    }
    assert!(
        not_gated.is_empty(),
        "these negative controls are NOT currently ask/deny — pre-existing gap, \
report it rather than treat it as passing: {not_gated:#?}"
    );
}
