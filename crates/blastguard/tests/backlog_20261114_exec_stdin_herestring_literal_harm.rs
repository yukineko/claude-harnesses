#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (observed 2026-11-14 by another agent): `exec <<< 'chmod 777 /etc/hosts';
//! bash` was reported Ask in main mode.
//!
//! Two readings: (1) real defect: the program text is a literal on the command
//! line (the here-string) and it is system harm; user ruling is that literal
//! system harm is Deny in main too; (2) intended: exec-redirected stdin is
//! undeterminable so Ask. Tell apart: the user ruling (system harm always
//! Deny) decides it; the control shows the direct form's verdict.
//!
//! Asserted: deny in both main and subagent mode.
//!
//! DOES NOT PROVE: non-harmful here-string programs (Ask is fine there), other
//! harm literals. Control asserts only that the direct harm command is deny.
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

#[test]
#[ignore = "RED: 2026-11-14 exec stdin herestring literal harm is Ask in main — remove the ignore when fixed"]
fn exec_stdin_herestring_literal_system_harm_is_deny_in_both_modes() {
    let home = unique_home("red");
    let cmd = "exec <<< 'chmod 777 /etc/hosts'; bash";
    let (m, mr) = run_as(cmd, &home, false);
    let (s, sr) = run_as(cmd, &home, true);
    assert_eq!(m, "deny", "main reason={mr}");
    assert_eq!(s, "deny", "subagent reason={sr}");
}

#[test]
fn control_direct_chmod_etc_hosts_is_deny() {
    let home = unique_home("ctl");
    let (d, r) = run_as("chmod 777 /etc/hosts", &home, true);
    assert_eq!(d, "deny", "reason={r}");
}
