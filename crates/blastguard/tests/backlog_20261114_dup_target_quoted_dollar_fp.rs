#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (fail-open, FP in quote-aware check, observed 2026-11-15 by verifier on base 091f4ab87):
//! `bash <&'$fd'` and `bash <&\$fd` (single-quoted / backslash-escaped $, so target is the
//! LITERAL string `$fd`, not an expansion) are now ask (main) / deny (subagent), but should be
//! allow in both modes.
//!
//! Two readings: (1) real FP: real bash rejects both with "bash: $fd: ambiguous redirect" rc=1
//! because there is no fd named `$fd`. The shell never expands; the literal string is not a
//! valid fd number. Since expansion is prevented by the quote, no actual FD lookup or redirection
//! happens. (2) intended: the detector was overly strict. Cause: exec_fds.rs:629
//! `(target.contains('$') || target.contains('`'))` is not quote-aware; single-quoted and
//! backslash-escaped $ should be ignored.
//!
//! Asserted: main == "allow", subagent == "allow", for both command shapes.
//! Controls (unquoted double-quoted, which DO expand): both return not allow (main) / deny (subagent).
//!
//! DOES NOT PROVE: why the detector treats unquoted and double-quoted differently, behaviour
//! with $$ or other expansions inside quotes, exact ast structure of bash's parse tree.
//! Controls show the distinction exists (single/backslash vs double/unquoted differ in verdict).
//!
//! RED written by an independent verifier, not the implementer.

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
        "bg-quoted-1114-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "quoted-dollar-1114",
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

fn check_allow(cmd: &str, tag: &str) {
    let home = unique_home(tag);
    let (m, mr) = run_as(cmd, &home, false);
    let (s, sr) = run_as(cmd, &home, true);
    assert_eq!(
        m, "allow",
        "main must allow quoted target; cmd={cmd:?} got={m} reason={mr}"
    );
    assert_eq!(s, "allow", "subagent must allow quoted target; cmd={cmd:?} reason={sr}");
}

fn check_not_allow(cmd: &str, tag: &str) {
    let home = unique_home(tag);
    let (m, mr) = run_as(cmd, &home, false);
    let (s, sr) = run_as(cmd, &home, true);
    assert!(
        m != "allow",
        "main must not allow unquoted target; cmd={cmd:?} got={m} reason={mr}"
    );
    assert_eq!(s, "deny", "subagent must deny unquoted target; cmd={cmd:?} reason={sr}");
}

#[test]
#[ignore = "RED: 2026-11-14 quoted $ in dup target treated as expansion — remove the ignore when fixed"]
fn bash_dup_single_quoted_dollar_fd_should_allow() {
    check_allow("bash <&'$fd'", "single-quoted");
}

#[test]
#[ignore = "RED: 2026-11-14 quoted $ in dup target treated as expansion — remove the ignore when fixed"]
fn bash_dup_backslash_escaped_dollar_fd_should_allow() {
    check_allow("bash <&\\$fd", "backslash-escaped");
}

#[test]
fn control_bash_dup_unquoted_dollar_fd_is_not_allow() {
    check_not_allow("bash <&$fd", "ctrl-unquoted");
}

#[test]
fn control_bash_dup_double_quoted_dollar_fd_is_not_allow() {
    check_not_allow("bash <&\"$fd\"", "ctrl-double-quoted");
}
