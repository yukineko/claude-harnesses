#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (fail-open, observed 2026-11-14 by another agent): `bash /dev/fd/3` (no
//! `exec` on the line) was reported Allow in main and subagent mode, while
//! `bash <&3` is Ask in main.
//!
//! Two readings: (1) real defect: /dev/fd/3 is an inherited descriptor whose
//! content blastguard cannot read, the same undeterminable program source as
//! `bash <&3`; (2) intended: /dev/fd/N operands are treated as ordinary paths.
//! Tell apart: the control `bash <&3` (same fd, same unreadable program) is
//! observed non-allow in main, so the operand form being allow is inconsistent.
//!
//! Asserted: main != "allow", subagent == "deny" for `bash /dev/fd/3`; control
//! asserts `bash <&3` is Ask in main (observed).
//!
//! DOES NOT PROVE: fds other than 3, `source /dev/fd/3`, the subagent verdict
//! of `bash <&3`.
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

fn wt(home: &Home) -> String {
    home.0.join("wt").display().to_string()
}

#[test]
#[ignore = "RED: 2026-11-14 bash /dev/fd/3 allowed — remove the ignore when fixed"]
fn bash_dev_fd_operand_is_not_allowed() {
    let home = unique_home("red");
    let cmd = "bash /dev/fd/3";
    let (m, mr) = run_as(cmd, &home, false);
    let (s, sr) = run_as(cmd, &home, true);
    assert!(m != "allow", "main must not allow; got={m} reason={mr}");
    assert_eq!(s, "deny", "subagent must deny; reason={sr}");
}

#[test]
fn control_bash_dup_fd3_is_ask_in_main() {
    let home = unique_home("ctl");
    let (d, r) = run_as("bash <&3", &home, false);
    assert_eq!(d, "ask", "observed Ask in main; reason={r}");
}
