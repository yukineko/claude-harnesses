#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (system harm Ask in main, observed 2026-11-14 in the task worktrees of
//! run-20261114-111444-69657, base b0b4ac6e3 family): a variable assigned a
//! system file, then used by a delete that is NOT the first/only statement of a
//! command substitution, is Ask in main mode (Deny in subagent mode only
//! because subagents cannot answer an Ask). Observed (main / subagent):
//!   L=/etc/hosts; x=$(rm -f $L)                  deny / deny  (control)
//!   L=/etc/hosts; x=$(echo a; rm -f $L)          ask  / deny  <- main Ask
//!   L=/etc/hosts; x=$( { rm -f $L; } )           ask  / deny  <- main Ask
//!   L=/etc/hosts; x=$(echo $(rm -f $L))          ask  / deny  <- main Ask
//!
//! Asserted: all three shapes are Deny in main AND subagent mode (system harm
//! is Deny in main as well, user ruling). Control: the single-statement
//! substitution is Deny in both modes.
//!
//! DOES NOT PROVE: other harmful verbs (chmod/mv/truncate), backtick
//! substitutions, process substitution, or that the subagent Deny is for the
//! system-harm reason (subagent mode denies every Ask).
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
        "bg-varmultisub-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "var-multi-statement-subst-20261114",
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

fn undenied(cmds: &[&str], tag: &str) -> Vec<String> {
    let home = unique_home(tag);
    let mut bad = Vec::new();
    for cmd in cmds {
        for subagent in [false, true] {
            let (d, r) = run_as(cmd, &home, subagent);
            if d != "deny" {
                bad.push(format!("subagent={subagent} {cmd:?} => {d} ({r})"));
            }
        }
    }
    bad
}

#[test]
#[ignore = "RED: 2026-11-14 system-file var deleted in a non-first statement of $(...) is main Ask, not Deny — remove the ignore when fixed"]
fn system_file_var_deleted_inside_multi_statement_substitution_is_deny() {
    let cmds = [
        "L=/etc/hosts; x=$(echo a; rm -f $L)",
        "L=/etc/hosts; x=$( { rm -f $L; } )",
        "L=/etc/hosts; x=$(echo $(rm -f $L))",
    ];
    let bad = undenied(&cmds, "red");
    assert!(
        bad.is_empty(),
        "expected Deny for every shape:\n{}",
        bad.join("\n")
    );
}

#[test]
fn control_system_file_var_deleted_in_single_statement_substitution_is_deny() {
    let cmds = ["L=/etc/hosts; x=$(rm -f $L)"];
    let bad = undenied(&cmds, "ctl");
    assert!(
        bad.is_empty(),
        "control shapes must stay Deny:\n{}",
        bad.join("\n")
    );
}
