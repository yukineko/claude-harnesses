#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED for spec blastguard-comment-collapse-residual R2 (backlog e4523bfd,
//! 2026-11-13). Written by a non-implementer spec-prep agent.
//!
//! `env -S '<payload>'` whose operand carries an interpreter program with a
//! `#` comment line before a system-harm call (os.chmod('/etc/hosts',0)).
//! R2 allows either splitting the operand by env's rules or refusing to
//! judge it; in both cases the verdict must not be Allow (main: Ask or Deny;
//! subagent: Deny).
//!
//! Asserted (RED): main is not Allow; subagent is Deny.
//! Control (not ignored): a harmless `env -S 'ls -la'` stays Allow, so the
//! RED is about the payload, not about `env -S` being refused wholesale.
//!
//! DOES NOT PROVE: whether the fix splits or refuses; the `--split-string`
//! spelling (covered by daf7611b_executor_behaviour.rs for other payloads);
//! the doc-comment / README rewrites R2's acceptance also requires.

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
        "bg-envS-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

/// Run the hook; returns the permissionDecision ("allow" on empty stdout).
fn decide(command: &str, home: &Home, subagent: bool) -> String {
    let mut p = serde_json::json!({
        "session_id": "env-split-string-comment-collapse-20261113",
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
    println!("[{command:?}] subagent={subagent} => {:?}", stdout.trim());
    if stdout.trim().is_empty() {
        return "allow".into();
    }
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    v["hookSpecificOutput"]["permissionDecision"]
        .as_str()
        .unwrap_or("?")
        .to_string()
}

const WITH_COMMENT: &str = "env -S 'python3 -c \"import os # x\nos.chmod(\\\"/etc/hosts\\\",0)\"'";

// test-weakening-justified: new RED for spec blastguard-comment-collapse-residual
// R2 (backlog e4523bfd, 2026-11-13); the ignore is removed in the fixing commit.
#[test]
#[ignore = "RED 2026-11-13: env -S operand re-joined on one line hides a system-harm call after a # comment (spec blastguard-comment-collapse-residual R2)"]
fn env_split_string_payload_with_comment_is_not_allowed() {
    let home = unique_home("red");
    let main = decide(WITH_COMMENT, &home, false);
    let sub = decide(WITH_COMMENT, &home, true);
    assert!(
        main == "ask" || main == "deny",
        "main session must be Ask or Deny, got {main}: {WITH_COMMENT:?}"
    );
    assert_eq!(
        sub, "deny",
        "subagent must be Deny, got {sub}: {WITH_COMMENT:?}"
    );
}

#[test]
fn control_harmless_env_split_string_stays_allowed() {
    let home = unique_home("ctl");
    assert_eq!(decide("env -S 'ls -la'", &home, false), "allow");
}
