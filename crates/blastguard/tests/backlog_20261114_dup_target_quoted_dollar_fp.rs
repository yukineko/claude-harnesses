#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! History: written 2026-11-14/15 (base 091f4ab87) as a RED expecting allow for `bash <&'$fd'`
//! and `bash <&\$fd` (single-quoted / backslash-escaped `$`, so the dup target is the LITERAL
//! string `$fd`), on the reading that the detector was not quote-aware (exec_fds.rs).
//!
//! RE-ANCHORED (backlog 796d27b6): the later user ruling T2 (2026-11-18, SUBSET.md item 23)
//! says "A program read from a file descriptor (`bash <&'$fd'`) is Undetermined." Undetermined
//! resolves to ask in main and deny in a subagent. The original allow expectation is
//! superseded; the gate was not changed.
//!
//! Asserted now: for both command shapes, main == "ask" and subagent == "deny".
//! Controls (unquoted / double-quoted, which DO expand): main != allow, subagent == deny.
//!
//! DOES NOT PROVE: behaviour with $$ or other expansions inside quotes, exact ast structure of
//! bash's parse tree, or that quote-awareness is irrelevant for non-program fd redirections.
//!
//! Original RED written by an independent verifier, not the implementer.

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

// T2 (2026-11-18, SUBSET.md item 23): "A program read from a file descriptor
// (`bash <&'$fd'`) is Undetermined." This supersedes the 2026-11-14 expectation of allow.
// Undetermined resolves to ask in main and deny in a subagent.
fn check_undetermined(cmd: &str, tag: &str) {
    let home = unique_home(tag);
    let (m, mr) = run_as(cmd, &home, false);
    let (s, sr) = run_as(cmd, &home, true);
    // T2 (2026-11-18, SUBSET.md item 23): main = ask (was allow).
    assert_eq!(
        m, "ask",
        "T2: main must ask (Undetermined) for program read from fd; cmd={cmd:?} got={m} reason={mr}"
    );
    // T2 (2026-11-18, SUBSET.md item 23): subagent = deny (was allow).
    assert_eq!(
        s, "deny",
        "T2: subagent must deny (Undetermined) for program read from fd; cmd={cmd:?} reason={sr}"
    );
}

fn check_not_allow(cmd: &str, tag: &str) {
    let home = unique_home(tag);
    let (m, mr) = run_as(cmd, &home, false);
    let (s, sr) = run_as(cmd, &home, true);
    assert!(
        m != "allow",
        "main must not allow unquoted target; cmd={cmd:?} got={m} reason={mr}"
    );
    assert_eq!(
        s, "deny",
        "subagent must deny unquoted target; cmd={cmd:?} reason={sr}"
    );
}

#[test]
fn bash_dup_single_quoted_dollar_fd_is_undetermined_per_t2() {
    // T2 (2026-11-18, SUBSET.md item 23): Undetermined, not allow.
    check_undetermined("bash <&'$fd'", "single-quoted");
}

#[test]
fn bash_dup_backslash_escaped_dollar_fd_is_undetermined_per_t2() {
    // T2 (2026-11-18, SUBSET.md item 23): Undetermined, not allow.
    check_undetermined("bash <&\\$fd", "backslash-escaped");
}

#[test]
fn control_bash_dup_unquoted_dollar_fd_is_not_allow() {
    check_not_allow("bash <&$fd", "ctrl-unquoted");
}

#[test]
fn control_bash_dup_double_quoted_dollar_fd_is_not_allow() {
    check_not_allow("bash <&\"$fd\"", "ctrl-double-quoted");
}
