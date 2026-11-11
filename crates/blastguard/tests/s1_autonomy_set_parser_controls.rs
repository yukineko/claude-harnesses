#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! S1 parser-based controls (backlog 12ba5845). Recognition of
//! `condukt state autonomy-set on` must go through the real shell parser:
//! text inside a quoted string must not trigger, while wrappers
//! (`bash -c '...'`, `env FOO=1 ...`) must.

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
        "bg-s1parser-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    Home(dir)
}

/// Run the hook; `subagent` adds a non-empty `agent_id`. Returns (decision, reason).
fn run(command: &str, cwd: &std::path::Path, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "s1-parser-20261112",
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

fn decide(command: &str, subagent: bool) -> (String, String) {
    let home = unique_home("p");
    run(
        command,
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")),
        &home,
        subagent,
    )
}

#[test]
fn s1_parser_quoted_text_is_not_denied_interactive() {
    let c = r#"echo "condukt state autonomy-set on""#;
    let (d, r) = decide(c, false);
    assert_ne!(d, "deny", "{c} (interactive): {d} | {r}");
}

#[test]
fn s1_parser_quoted_text_is_not_denied_subagent() {
    let c = r#"echo "condukt state autonomy-set on""#;
    let (d, r) = decide(c, true);
    assert_ne!(d, "deny", "{c} (subagent): {d} | {r}");
}

#[test]
fn s1_parser_bash_c_wrapper_subagent_is_denied() {
    let c = "bash -c 'condukt state autonomy-set on'";
    let (d, r) = decide(c, true);
    assert_eq!(d, "deny", "{c} (subagent): {d} | {r}");
}

#[test]
fn s1_parser_bash_c_wrapper_interactive_is_not_allowed() {
    let c = "bash -c 'condukt state autonomy-set on'";
    let (d, r) = decide(c, false);
    assert_ne!(d, "allow", "{c} (interactive): {d} | {r}");
}

#[test]
fn s1_parser_env_prefix_wrapper_subagent_is_denied() {
    let c = "env FOO=1 condukt state autonomy-set on";
    let (d, r) = decide(c, true);
    assert_eq!(d, "deny", "{c} (subagent): {d} | {r}");
}
