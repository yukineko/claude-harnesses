#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Control, not a RED (backlog 54231a0a): all 3 pairs already pass at main
//! b14310c00 (measured 2026-11-17); the RED for 54231a0a is
//! `statically_resolved_fd_var_gets_literal_verdict` in
//! backlog_20261114_exec_fd_dynamic_fd_reader.rs. A statically resolved `$fd` (e.g. `fd=3; ...`) must be
//! judged exactly like the literal fd form (user ruling 2026-11-16 (A);
//! blastguard rewrites `$fd`->`3` before any rule).
//!
//! Pure consistency test: each test takes a (literal, var-resolved) pair and
//! asserts, in both main and subagent, that the var form's decision EQUALS the
//! literal form's decision. No fixed expected value is asserted, because the
//! literal verdict is whatever blastguard decides for the literal command.
//! The plain `$fd` pairs for `bash <&$fd` and `bash /dev/fd/$fd` live in
//! backlog_20261114_exec_fd_dynamic_fd_reader.rs.
//!
//! DOES NOT PROVE: that the shared verdict is correct (both could be equally
//! wrong), other fd numbers, `exec 3< file`, other shells, dynamic fd values
//! such as `$((1+2))` or inherited `$fd`, harmful payloads, or the root cause.
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
        "bg-fdvar-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "static-fd-var-matches-literal-20261116",
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

fn check_pair(tag: &str, literal: &str, var: &str) {
    let home = unique_home(tag);
    for subagent in [false, true] {
        let (dl, rl) = run_as(literal, &home, subagent);
        let (dv, rv) = run_as(var, &home, subagent);
        assert_eq!(
            dl, dv,
            "subagent={subagent}: literal {literal:?} => {dl} ({rl}) but var {var:?} => {dv} ({rv})"
        );
    }
}

#[test]
fn bash_stdin_dup_fd_var_braced() {
    check_pair(
        "dupbr",
        "exec 3<<< 'echo hi'; bash <&3",
        "fd=3; exec 3<<< 'echo hi'; bash <&${fd}",
    );
}

#[test]
fn sh_zero_dup_fd_var() {
    check_pair(
        "zero",
        "exec 3<<< 'echo hi'; sh 0<&3",
        "fd=3; exec 3<<< 'echo hi'; sh 0<&$fd",
    );
}

#[test]
fn heredoc_exec_fd_var() {
    check_pair(
        "heredoc",
        "exec 3<<EOF\necho hi\nEOF\nbash <&3",
        "exec 3<<EOF\necho hi\nEOF\nfd=3; bash <&$fd",
    );
}
