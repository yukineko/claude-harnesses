#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (fail-open, observed 2026-11-14 by another agent on base 56084c672):
//! `fd=3; exec 3<<< 'chmod 777 /etc/hosts'; bash <&$fd` and plain `bash <&$fd`
//! were reported Allow in main and subagent mode.
//!
//! Two readings: (1) real defect: `<&$fd` duplicates stdin from an fd whose
//! number comes from an expansion, so bash's program source is undeterminable
//! and must resolve to Ask (main) / Deny (subagent) like `bash <&3` does;
//! (2) intended: fd-dup from a variable is deliberately allowed. Tell apart: the
//! literal-fd form `bash <&3` is not allow, so allowing the variable form is
//! inconsistent -> reading (1).
//!
//! Asserted: main != "allow", subagent == "deny", for both command shapes.
//!
//! DOES NOT PROVE: the literal-harm variant's exact reason text, verdict for fd
//! numbers other than 3, behaviour with the shell's real fd table. Control only
//! shows an unrelated `fd=3; echo $fd` is allow.
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

fn check(cmd: &str, tag: &str) {
    let home = unique_home(tag);
    let (m, mr) = run_as(cmd, &home, false);
    let (s, sr) = run_as(cmd, &home, true);
    assert!(
        m != "allow",
        "main must not allow; cmd={cmd:?} got={m} reason={mr}"
    );
    assert_eq!(s, "deny", "subagent must deny; cmd={cmd:?} reason={sr}");
}

#[test]
#[ignore = "RED: 2026-11-14 bash <&$fd allowed — remove the ignore when fixed"]
fn exec_herestring_then_bash_dup_var_fd_is_not_allowed() {
    check("fd=3; exec 3<<< 'chmod 777 /etc/hosts'; bash <&$fd", "a");
}

#[test]
#[ignore = "RED: 2026-11-14 plain bash <&$fd allowed — remove the ignore when fixed"]
fn plain_bash_dup_var_fd_is_not_allowed() {
    check("bash <&$fd", "b");
}

#[test]
fn control_assign_and_echo_fd_is_allow() {
    let home = unique_home("ctl");
    let (d, r) = run_as("fd=3; echo $fd", &home, true);
    assert_eq!(d, "allow", "control; reason={r}");
}
