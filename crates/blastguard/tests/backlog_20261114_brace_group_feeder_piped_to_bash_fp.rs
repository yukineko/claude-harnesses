#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (false positive, p1, observed 2026-11-14 on blastguard 0.2.565 at
//! dc9b00c7b): a brace-group feeder that only echoes harmless text, piped into
//! bash, is Ask (main) / Deny (subagent), while the equivalent plain feeder is
//! Allow. Observed (main / subagent):
//!   echo 'echo hi' | bash               allow / allow  (control)
//!   { echo 'echo hi'; } | bash          ask   / deny   <- FP
//!   { echo a; echo b; } | bash          ask   / deny   <- FP
//! Two readings: (FP) the group's emitted text is statically known and
//! harmless, so Allow like the plain feeder; (intended) a multi-command group
//! feeding a shell is deliberately not modelled and Ask is fail-closed. Per the
//! repo ruling "gate FP = bug", this RED asserts Allow for the harmless forms.
//!
//! DOES NOT PROVE: that a harmful group feeder (`{ echo 'chmod 777 /etc/hosts'; } | bash`)
//! is Deny — it is not asserted here; a fix must not make it Allow.
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
        "bg-groupfeed-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "brace-group-feeder-fp-20261114",
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

fn not_allowed(cmds: &[&str], tag: &str) -> Vec<String> {
    let home = unique_home(tag);
    let mut bad = Vec::new();
    for cmd in cmds {
        for subagent in [false, true] {
            let (d, r) = run_as(cmd, &home, subagent);
            if d != "allow" {
                bad.push(format!("subagent={subagent} {cmd:?} => {d} ({r})"));
            }
        }
    }
    bad
}

#[test]
#[ignore = "RED: 2026-11-14 harmless brace-group feeder piped to bash is Ask/Deny (FP) — remove the ignore when fixed"]
fn harmless_brace_group_feeder_piped_to_bash_is_allow() {
    let bad = not_allowed(
        &["{ echo 'echo hi'; } | bash", "{ echo a; echo b; } | bash"],
        "red",
    );
    assert!(bad.is_empty(), "expected Allow:\n{}", bad.join("\n"));
}

#[test]
fn control_plain_harmless_feeder_piped_to_bash_is_allow() {
    let bad = not_allowed(&["echo 'echo hi' | bash"], "ctl");
    assert!(
        bad.is_empty(),
        "control must stay Allow:\n{}",
        bad.join("\n")
    );
}
