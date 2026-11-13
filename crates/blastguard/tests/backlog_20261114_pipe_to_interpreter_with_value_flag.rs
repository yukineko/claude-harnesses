#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (system-harm fail-open, p0, observed 2026-11-14 on blastguard 0.2.565 at
//! dc9b00c7b): a program that makes /etc/hosts world-writable, fed on stdin to
//! an interpreter that carries a value-taking option, is Allow in main and
//! subagent mode; the bare interpreter is Deny. Observed (main / subagent):
//!   echo 'import os; os.chmod("/etc/hosts", 0o777)' | python3        deny  / deny  (control)
//!   ... | python3 -X dev                                              allow / allow <- fail-open
//!   ... | python3 -W ignore                                           allow / allow <- fail-open
//!   echo 'require("fs").chmodSync("/etc/hosts", 0o777)' | node       deny  / deny  (control)
//!   ... | node -r m                                                   allow / allow <- fail-open
//! Sibling of backlog_20261114_pipe_to_shell_with_value_flags_or_s_args (same
//! mechanism family for shells; here the interpreters).
//!
//! DOES NOT PROVE: other interpreters (perl/ruby/deno), other flags, or that a
//! real script-file operand (`python3 script.py`) stays handled.
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
        "bg-interpflags-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "pipe-interp-value-flags-20261114",
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

const PY: &str = "echo 'import os; os.chmod(\"/etc/hosts\", 0o777)' | ";
const NODE: &str = "echo 'require(\"fs\").chmodSync(\"/etc/hosts\", 0o777)' | ";

fn undenied(cmds: &[String], tag: &str) -> Vec<String> {
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
#[ignore = "RED: 2026-11-14 harmful program piped to python3/node with a value-taking flag is Allow — remove the ignore when fixed"]
fn harmful_program_piped_to_interpreter_with_value_flag_is_deny() {
    let cmds = vec![
        format!("{PY}python3 -X dev"),
        format!("{PY}python3 -W ignore"),
        format!("{NODE}node -r m"),
    ];
    let bad = undenied(&cmds, "red");
    assert!(
        bad.is_empty(),
        "expected Deny for every shape:\n{}",
        bad.join("\n")
    );
}

#[test]
fn control_harmful_program_piped_to_bare_interpreter_is_deny() {
    let cmds = vec![format!("{PY}python3"), format!("{NODE}node")];
    let bad = undenied(&cmds, "ctl");
    assert!(
        bad.is_empty(),
        "control shapes must stay Deny:\n{}",
        bad.join("\n")
    );
}
