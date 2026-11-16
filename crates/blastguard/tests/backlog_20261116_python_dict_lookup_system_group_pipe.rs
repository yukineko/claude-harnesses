#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (fail-open, observed 2026-11-16 at main fa58e3698, run-20261116-143410-12142):
//! in a `python3 - <<'EOF'` heredoc, `os.__dict__['sys'+'tem']("cat x | { bash; }")`
//! is judged allow in main and subagent mode. The dict lookup spells the runner
//! name by concatenation, so no runner marker (`os.system`, `getattr`) appears.
//! The plain `os.system("cat x | { bash; }")` twin is ask/deny.
//!
//! Asserted: not allow in main and subagent mode.
//!
//! DOES NOT PROVE: other obfuscations (`getattr(os,'sys'+'tem')`, `vars(os)`,
//! `importlib`), nor the reason text. The control only shows the plain spelling
//! is not allow.
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
        "bg-pydict1116-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "bg-pydict-20261116",
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

fn assert_not_allow(tag: &str, cmd: &str) {
    let home = unique_home(tag);
    let (m, mr) = run_as(cmd, &home, false);
    let (s, sr) = run_as(cmd, &home, true);
    assert!(
        m != "allow" && s != "allow",
        "{tag}: main={m} [{mr}] subagent={s} [{sr}]; cmd={cmd:?}"
    );
}

#[test]
#[ignore = "RED: python heredoc os.__dict__['sys'+'tem'](\"cat x | { bash; }\") judged allow (fail-open); plain os.system twin is ask/deny"]
fn dict_lookup_obfuscated_system_group_pipe_is_not_allow() {
    assert_not_allow(
        "dict",
        "python3 - <<'EOF'\nimport os\nos.__dict__['sys'+'tem'](\"cat x | { bash; }\")\nEOF",
    );
}

#[test]
fn control_plain_os_system_group_pipe_is_not_allow() {
    assert_not_allow(
        "ctl",
        "python3 - <<'EOF'\nimport os\nos.system(\"cat x | { bash; }\")\nEOF",
    );
}
