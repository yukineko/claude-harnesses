// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `DataView::floor` (`src/detect/heredoc_data.rs`), the Ask path. Independent
//! test author (CLAUDE.md §2(a)); verdicts only, through the real hook binary.
//!
//! The condition the floor exists for: a quoted heredoc body is data and is
//! removed (step 0a), but another command of the line has an UNDETERMINED word
//! (`frobnicate $X`) that may name the file the body lands in. Then
//!
//! * the line judged with the body removed is Allow, and
//! * the line judged with the body kept (`whole`) is not Allow
//!
//! → Ask (a refusal to guess), not Allow and not the kept body's Deny. The
//! pair: when `whole` is Allow too (a benign body) the floor leaves Allow.
//! And when no word is undetermined (`frobnicate y`) the floor does not
//! apply, so the same harmful-looking body stays Allow — that pins that the
//! Ask comes from the undetermined word and not from reading the body.

#[path = "support/bounded.rs"]
mod bounded;

use std::process::{Command, Stdio};

/// Run the hook; `interactive` = a CLI session where Ask is available
/// (`CLAUDECODE=1` + `CLAUDE_CODE_ENTRYPOINT=cli`), else Ask hardens to Deny.
fn verdict(command: &str, interactive: bool) -> (String, String) {
    let base = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("heredoc_data_floor_ask_{}", std::process::id()));
    let home = base.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let payload = serde_json::json!({
        "session_id": "heredoc-data-floor-ask",
        "cwd": "/private/tmp",
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": {"command": command},
    })
    .to_string();
    let mut hook = Command::new(env!("CARGO_BIN_EXE_blastguard"));
    hook.env_clear()
        .env("HOME", &home)
        .env("PATH", "/usr/bin:/bin")
        .env("CLAUDECODE", "1")
        .env("BLASTGUARD_APPROVALS_DIR", base.join("store"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if interactive {
        hook.env("CLAUDE_CODE_ENTRYPOINT", "cli");
    }
    let out = bounded::run_bounded(&mut hook, payload.as_bytes(), bounded::HOOK_DEADLINE);
    assert_eq!(
        out.status.code(),
        Some(0),
        "hook must exit 0 for {command:?}"
    );
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if stdout.is_empty() {
        return ("allow".into(), String::new());
    }
    let doc: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let h = &doc["hookSpecificOutput"];
    (
        h["permissionDecision"].as_str().unwrap().to_string(),
        h["permissionDecisionReason"]
            .as_str()
            .unwrap_or("")
            .to_string(),
    )
}

/// Body that is Deny when judged as shell (a system-directory delete).
const HARMFUL: &str = "rm -rf /usr/lib";

#[test]
fn undetermined_word_and_non_allow_whole_line_floors_to_ask() {
    let cmd = format!("cat > notes.md <<'EOF'\n{HARMFUL}\nEOF\nfrobnicate $X");
    let (d, r) = verdict(&cmd, true);
    assert_eq!(
        d, "ask",
        "interactive: floor must Ask; {cmd:?} => {d} | {r}"
    );
    // Subagent column: the same Ask hardens to Deny.
    let (d, r) = verdict(&cmd, false);
    assert_eq!(
        d, "deny",
        "non-interactive: Ask hardens; {cmd:?} => {d} | {r}"
    );
}

#[test]
fn undetermined_word_and_allow_whole_line_stays_allow() {
    let cmd = "cat > notes.md <<'EOF'\nhello\nEOF\nfrobnicate $X";
    let (d, r) = verdict(cmd, true);
    assert_eq!(
        d, "allow",
        "whole line Allow: floor keeps Allow; {cmd:?} => {d} | {r}"
    );
}

#[test]
fn no_undetermined_word_does_not_floor() {
    let cmd = format!("cat > notes.md <<'EOF'\n{HARMFUL}\nEOF\nfrobnicate y");
    let (d, r) = verdict(&cmd, true);
    assert_eq!(
        d, "allow",
        "no undetermined word: body is data; {cmd:?} => {d} | {r}"
    );
}
