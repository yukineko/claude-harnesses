#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (system-harm, p0 — ambiguous, see below; observed 2026-11-14 on
//! blastguard 0.2.565 at dc9b00c7b): the harmful program is bound to fd 3 by a
//! separate `exec` statement and a later `bash <&3` executes it. Observed
//! (main / subagent):
//!   exec 3<<< 'chmod 777 /etc/hosts'; bash <&3          ask  / deny
//!   bash 3<<< 'chmod 777 /etc/hosts' <&3                deny / deny  (control)
//! Policy: a system-harm program reaching a shell is Deny in main too; Ask is
//! the answer for an UNDETERMINABLE program, but here the program text is
//! literally on the command line. Two readings: (real defect) the fd-3 text is
//! resolvable across statements, so main must be Deny like the in-stage form;
//! (intended) cross-statement fd tracking is out of model and Ask is the
//! deliberate fail-closed answer. This RED asserts the first reading.
//!
//! DOES NOT PROVE: other fd numbers, `exec 3< file`, here-docs.
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
        "bg-execfd-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "exec-fd-herestring-20261114",
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
#[ignore = "RED: 2026-11-14 exec 3<<< harmful; bash <&3 is Ask in main (expected Deny) — remove the ignore when fixed"]
fn exec_fd_herestring_then_shell_reading_fd_is_deny_in_main() {
    let home = unique_home("red");
    let cmd = "exec 3<<< 'chmod 777 /etc/hosts'; bash <&3";
    let (d, r) = run_as(cmd, &home, false);
    assert_eq!(d, "deny", "main: {cmd:?} => {d} ({r})");
    let (d, r) = run_as(cmd, &home, true);
    assert_eq!(d, "deny", "subagent: {cmd:?} => {d} ({r})");
}

#[test]
fn control_in_stage_fd_herestring_is_deny() {
    let home = unique_home("ctl");
    let cmd = "bash 3<<< 'chmod 777 /etc/hosts' <&3";
    for subagent in [false, true] {
        let (d, r) = run_as(cmd, &home, subagent);
        assert_eq!(d, "deny", "subagent={subagent}: {cmd:?} => {d} ({r})");
    }
}
