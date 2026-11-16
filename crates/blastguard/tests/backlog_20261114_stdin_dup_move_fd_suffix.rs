#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (candidate fail-open): detect/payload.rs procsub_programs records a
//! `<&TARGET` stdin dup only when TARGET parses as u32. `bash <&3-` (move fd 3
//! to stdin) has target "3-", which does not parse, so the dup is not recorded.
//!
//! Two readings: (1) real defect: `<&3-` is the same stdin source as `<&3`, so
//! it must get the same verdict (non-Allow in main, Deny in subagent), and with
//! `exec 3<<< 'chmod 777 /etc/hosts'` before it must be Deny in both modes;
//! (2) intended: the move form is judged elsewhere. Tell apart: if `bash <&3` is
//! non-Allow but `bash <&3-` is Allow, the parse gap is real.
//!
//! DOES NOT PROVE: other fd numbers, `>&3-`, or the shell's real fd semantics.
//! Controls record the verdict of the `<&3` forms.
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
        "bg-side1114-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "side-findings-20261114",
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

fn assert_non_allow_main_deny_sub(cmd: &str, tag: &str) {
    let home = unique_home(tag);
    let (m, mr) = run_as(cmd, &home, false);
    let (s, sr) = run_as(cmd, &home, true);
    assert_ne!(m, "allow", "main; cmd={cmd:?} reason={mr}");
    assert_eq!(s, "deny", "subagent; cmd={cmd:?} got={s} reason={sr}");
}

fn assert_deny_both(cmd: &str, tag: &str) {
    let home = unique_home(tag);
    let (m, mr) = run_as(cmd, &home, false);
    let (s, sr) = run_as(cmd, &home, true);
    assert_eq!(m, "deny", "main; cmd={cmd:?} got={m} reason={mr}");
    assert_eq!(s, "deny", "subagent; cmd={cmd:?} got={s} reason={sr}");
}

#[test]
fn bash_move_fd_to_stdin_is_not_allowed() {
    assert_non_allow_main_deny_sub("bash <&3-", "a");
}

#[test]
fn bash_explicit_fd0_move_to_stdin_is_not_allowed() {
    assert_non_allow_main_deny_sub("bash 0<&3-", "b0");
}

// Not a RED: observed Deny in both modes at main dde31f11b (the literal-harm
// tracking already covers the `<&3-` spelling); kept as a regression guard.
#[test]
fn control_exec_herestring_then_bash_move_fd_literal_harm_denied() {
    assert_deny_both("exec 3<<< 'chmod 777 /etc/hosts'; bash <&3-", "b");
}

#[test]
fn control_bash_dup_fd_literal_is_not_allowed() {
    assert_non_allow_main_deny_sub("bash <&3", "c1");
}

#[test]
fn control_exec_herestring_then_bash_dup_fd_literal_harm_denied() {
    assert_deny_both("exec 3<<< 'chmod 777 /etc/hosts'; bash <&3", "c2");
}
