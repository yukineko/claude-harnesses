#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (fail-open, side finding F11 observed 2026-11-15 on base 0.2.603):
//! `exec 3<<<'chmod 777 /etc/hosts'; bash <&'3'` and `... bash <&"3"` were
//! observed allow in main and subagent mode, while the unquoted
//! `exec 3<<<'chmod 777 /etc/hosts'; bash <&3` is deny in both (reason: chmod
//! re-permissions /etc/hosts).
//!
//! Two readings: (1) real defect: bash removes the quotes before it applies
//! the redirection, so `<&'3'` and `<&"3"` are exactly `<&3`; the verdict must
//! match the `<&3` verdict (deny); (2) intended: a quoted fd number is
//! treated as undeterminable and allowed. Tell apart: reading (2) cannot be
//! intended because allow is the most permissive answer for an input that is
//! judged deny in its unquoted spelling, and an undeterminable fd is Ask/Deny
//! by the fail-closed rule, never allow -> reading (1).
//!
//! Asserted: deny in main and subagent mode for both quoted spellings.
//!
//! DOES NOT PROVE: fd numbers other than 3, harm other than `chmod 777
//! /etc/hosts`, `<&-` / `<&3-` move forms, or the reason text. The control
//! shows only that the unquoted `<&3` form is deny in both modes.
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
        "bg-side1115-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "side-findings-20261115",
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

fn assert_deny_both(cmd: &str, tag: &str) {
    let home = unique_home(tag);
    let (m, mr) = run_as(cmd, &home, false);
    let (s, sr) = run_as(cmd, &home, true);
    assert_eq!(m, "deny", "main must deny; cmd={cmd:?} reason={mr}");
    assert_eq!(s, "deny", "subagent must deny; cmd={cmd:?} reason={sr}");
}

#[test]
fn quoted_single_numeric_dup_target_matches_unquoted_verdict() {
    assert_deny_both("exec 3<<<'chmod 777 /etc/hosts'; bash <&'3'", "sq");
}

#[test]
fn quoted_double_numeric_dup_target_matches_unquoted_verdict() {
    assert_deny_both("exec 3<<<'chmod 777 /etc/hosts'; bash <&\"3\"", "dq");
}

#[test]
fn control_unquoted_numeric_dup_target_is_deny() {
    assert_deny_both("exec 3<<<'chmod 777 /etc/hosts'; bash <&3", "ctl");
}
