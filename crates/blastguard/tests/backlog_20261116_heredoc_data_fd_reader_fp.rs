#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (false positive; relay bgr-3c577fb491b6, subagent): blastguard DENIED a
//! command in which the line `exec 3<<< 'echo hi'; bash <&3` appears only as DATA
//! inside a quoted here-document (<<'EOF') fed to `while IFS= read -r c`. Reason
//! given: "`bash <&3` may take its program from stdin, and its stdin is duplicated
//! from fd 3, which `exec` opens on text blastguard does not read ...".
//! Expectation: the body is quoted data that no shell reads, so main = allow and
//! subagent != deny. Spec check (README.ja.md, docs/specs/blastguard.md): a
//! quoted-delimiter here-document body is not executed by the shell and its
//! redirect characters are data; the exec-fd tracking rules cover exec and reader
//! statements of the command, not body lines of a non-shell reader. Neither
//! document says such a body must be Ask.
//!
//! The verbatim relay command is NOT used as a test: the main session observed
//! Ask for it, and a main Ask becomes Deny in a subagent, so its outcome may stem
//! from something other than the data line (cause not localized). The minimal
//! while-read form is the RED.
//!
//! DOES NOT PROVE: that a body read by a real shell (bash <<'EOF', sh, eval) is
//! still judged; other fd numbers; unquoted here-documents; the verbatim relay
//! command; which pass causes the Deny (cause not localized).
//!
//! RED written by an independent test author, not the implementer.

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
        "bg-hdfd-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "heredoc-data-fd-reader-20261116",
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

const MINIMAL: &str = "while IFS= read -r c; do echo \"$c\"; done <<'EOF'\nexec 3<<< 'echo hi'; bash <&3\nEOF";
const CAT: &str = "cat <<'EOF'\nexec 3<<< 'echo hi'; bash <&3\nEOF";

fn check(tag: &str, cmd: &str) {
    let home = unique_home(tag);
    let (d, r) = run_as(cmd, &home, true);
    assert_ne!(d, "deny", "subagent: {cmd:?} => {d} ({r})");
    let (d, r) = run_as(cmd, &home, false);
    assert_eq!(d, "allow", "main: {cmd:?} => {d} ({r})");
}

#[test]
#[ignore = "RED: 2026-11-16 heredoc data line judged as an fd reader (bgr-3c577fb491b6) — remove when fixed"]
fn minimal_while_read_heredoc_data() {
    check("min", MINIMAL);
}

#[test]
fn cat_heredoc_data() {
    check("cat", CAT);
}
