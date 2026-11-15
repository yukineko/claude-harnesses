#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (false positive, observed 2026-11-14 at main 33464587): python
//! `shutil.copy2(SRC, DST)` is judged as WRITING its source: with a system
//! file as the SOURCE (`shutil.copy2('/etc/hosts','/tmp/x')`, which only
//! reads /etc/hosts) the main-session decision is Deny with the reason
//! "it writes `/etc/hosts` (`shutil.copy2(`)", while the same copy from a
//! non-system source (`shutil.copy2('/tmp/y','/tmp/x')`) is not Deny.
//!
//! Property asserted: the decision for a copy depends on its destination, not
//! on its (read-only) source: the system-source copy gets the same main-session
//! decision as the /tmp-source twin. Found by the 569b9b9d implementer
//! (reported as 別件, not changed); RED written by the flow-8cca7ef5
//! orchestrator, a non-implementer.
//!
//! DOES NOT PROVE: subagent mode (Ask hardens to Deny there, so it cannot tell
//! the two apart); other copy APIs; destinations in system directories.

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
        "bg-cps-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

/// Run the hook. Returns (decision, reason).
fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "copy-source-fp-20261113",
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

const SYSTEM_SOURCE: &str = "python3 -c \"import shutil; shutil.copy2('/etc/hosts','/tmp/x')\"";
const TMP_SOURCE: &str = "python3 -c \"import shutil; shutil.copy2('/tmp/y','/tmp/x')\"";

#[test]
#[ignore = "RED: shutil.copy2 source is judged as a write target (backlog p1 FP)"]
fn copy2_decision_does_not_depend_on_read_only_source() {
    let home = unique_home("fp");
    let (d_sys, r_sys) = run_as(SYSTEM_SOURCE, &home, false);
    let (d_tmp, r_tmp) = run_as(TMP_SOURCE, &home, false);
    assert_eq!(
        d_sys, d_tmp,
        "system-source copy2 got {d_sys} ({r_sys}) but /tmp-source twin got {d_tmp} ({r_tmp})"
    );
}

#[test]
fn control_tmp_source_copy2_is_not_denied_in_main_session() {
    let home = unique_home("ctl");
    let (d, r) = run_as(TMP_SOURCE, &home, false);
    assert_ne!(d, "deny", "control {TMP_SOURCE}: {d} {r}");
}
