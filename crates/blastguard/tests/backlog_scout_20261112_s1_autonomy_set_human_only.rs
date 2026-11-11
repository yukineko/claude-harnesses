#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Scout 2026-11-12 S1: a model-path `condukt state autonomy-set on` /
//! `CONDUKT_AUTONOMOUS=1 condukt policy answer ...` is a self-grant of autonomy
//! and must not be allowed. Observed at HEAD: allowed in the subagent payload.
//! Controls (`autonomy-set off`, `autonomy-check`) must stay non-deny.

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
        "bg-scout-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    Home(dir)
}

/// Run the hook; `subagent` adds a non-empty `agent_id`. Returns (decision, reason).
fn run(command: &str, cwd: &std::path::Path, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "scout-20261112",
        "cwd": cwd,
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

const ON: &str = "condukt state autonomy-set on";
const ENV_ANSWER: &str = "CONDUKT_AUTONOMOUS=1 condukt policy answer --id q1 --choice yes";

fn check(command: &str, subagent: bool, want_deny: bool) {
    let home = unique_home("s1");
    let (d, r) = run(
        command,
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")),
        &home,
        subagent,
    );
    if want_deny {
        assert_eq!(d, "deny", "{command} (subagent={subagent}): {d} | {r}");
    } else {
        assert_ne!(d, "allow", "{command} (subagent={subagent}): {d} | {r}");
    }
}

#[test]
#[ignore = "scout 2026-11-12 S1: RED, open defect. Remove the ignore when fixed"]
fn s1_autonomy_set_on_interactive_is_not_allowed() {
    check(ON, false, false);
}

#[test]
#[ignore = "scout 2026-11-12 S1: RED, open defect. Remove the ignore when fixed"]
fn s1_autonomy_set_on_subagent_is_denied() {
    check(ON, true, true);
}

#[test]
#[ignore = "scout 2026-11-12 S1: RED, open defect. Remove the ignore when fixed"]
fn s1_env_autonomous_policy_answer_interactive_is_not_allowed() {
    check(ENV_ANSWER, false, false);
}

#[test]
#[ignore = "scout 2026-11-12 S1: RED, open defect. Remove the ignore when fixed"]
fn s1_env_autonomous_policy_answer_subagent_is_denied() {
    check(ENV_ANSWER, true, true);
}

#[test]
fn s1_control_autonomy_set_off_stays_non_deny() {
    for sub in [false, true] {
        let home = unique_home("s1c");
        let (d, r) = run(
            "condukt state autonomy-set off",
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")),
            &home,
            sub,
        );
        assert_ne!(d, "deny", "autonomy-set off (subagent={sub}): {d} | {r}");
    }
}

#[test]
fn s1_control_autonomy_check_stays_non_deny() {
    for sub in [false, true] {
        let home = unique_home("s1k");
        let (d, r) = run(
            "condukt state autonomy-check",
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")),
            &home,
            sub,
        );
        assert_ne!(d, "deny", "autonomy-check (subagent={sub}): {d} | {r}");
    }
}
