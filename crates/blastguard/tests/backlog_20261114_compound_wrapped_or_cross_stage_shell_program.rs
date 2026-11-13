#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (system-harm fail-open, p0, observed 2026-11-14 by an independent
//! verifier and the orchestrator on blastguard builds 0e726e66 and 572cc1c3,
//! re-observed on this worktree's build, blastguard 0.2.541): a shell program
//! that is produced by a process substitution, a pipe or an fd redirect is not
//! judged when the shell reader sits inside a compound command (subshell,
//! brace group, if / while / for body), or when the program crosses a
//! statement boundary through fd 3, or when the shell is zsh with `=( ... )`.
//! Every shape below is Allow in BOTH main and subagent mode on this build:
//!
//!   (bash <(echo 'chmod 777 /etc/hosts'))
//!   { bash <(echo 'chmod 777 /etc/hosts') ; }
//!   if true; then bash <(echo 'chmod 777 /etc/hosts'); fi
//!   while true; do bash <(echo 'chmod 777 /etc/hosts'); done
//!   for i in 1; do bash <(echo 'chmod 777 /etc/hosts'); done
//!   exec 3< <(echo 'chmod 777 /etc/hosts'); bash <&3
//!   zsh =(echo 'chmod 777 /etc/hosts')
//!   (echo 'chmod 777 /etc/hosts' | bash)
//!   bash <(echo 'chmod 777 /etc/hosts')   (plain; Allow on this main-based
//!                                          build, the t2 procsub work is not here)
//!
//! Controls on this build: `printf 'chmod 777 /etc/hosts' | bash` and
//! `(chmod 777 /etc/hosts)` are Deny.
//!
//! Asserted (RED, `#[ignore]`d): every shape above is Deny in main mode and in
//! subagent mode (harming a system path is always Deny, never Ask).
//! Control (not ignored): the two Deny forms above stay Deny in both modes, so
//! the hook is wired and the RED fails on the shapes, not on the harness.
//!
//! DOES NOT PROVE: other readers (`sh`, `eval`, `source`), other compound
//! forms (`case`, `until`, functions, `&&` / `||` lists), other fds, or that
//! the Deny reason names the hidden program. It does not prove that a harmless
//! program in these shapes stays Allow (no false-positive guard here), and it
//! does not say which of the shapes share a root cause - the fix may need
//! several changes. Shapes that were already Deny on this build are not listed.
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
        "bg-cwcs-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

/// Run the hook. Returns (decision, reason).
fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "compound-cross-stage-20261114",
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

const SHAPES: &[&str] = &[
    "(bash <(echo 'chmod 777 /etc/hosts'))",
    "{ bash <(echo 'chmod 777 /etc/hosts') ; }",
    "if true; then bash <(echo 'chmod 777 /etc/hosts'); fi",
    "while true; do bash <(echo 'chmod 777 /etc/hosts'); done",
    "for i in 1; do bash <(echo 'chmod 777 /etc/hosts'); done",
    "exec 3< <(echo 'chmod 777 /etc/hosts'); bash <&3",
    "zsh =(echo 'chmod 777 /etc/hosts')",
    "(echo 'chmod 777 /etc/hosts' | bash)",
    "bash <(echo 'chmod 777 /etc/hosts')",
];

#[test]
#[ignore = "RED: 2026-11-14 system-harm program fed to a shell inside a compound command / via fd 3 / zsh =() / subshell pipe is Allow — remove the ignore when fixed"]
fn compound_wrapped_or_cross_stage_shell_program_is_deny() {
    let home = unique_home("red");
    let mut bad = Vec::new();
    for shape in SHAPES {
        for subagent in [false, true] {
            let (d, r) = run_as(shape, &home, subagent);
            if d != "deny" {
                bad.push(format!("subagent={subagent} {shape:?} => {d} ({r})"));
            }
        }
    }
    assert!(
        bad.is_empty(),
        "expected Deny for every shape:\n{}",
        bad.join("\n")
    );
}

#[test]
fn control_bare_pipe_to_bash_and_subshell_chmod_are_deny() {
    let home = unique_home("ctl");
    for subagent in [false, true] {
        for cmd in [
            "printf 'chmod 777 /etc/hosts' | bash",
            "(chmod 777 /etc/hosts)",
        ] {
            let (d, r) = run_as(cmd, &home, subagent);
            assert_eq!(d, "deny", "control {cmd:?} (subagent={subagent}): {d} {r}");
        }
    }
}
