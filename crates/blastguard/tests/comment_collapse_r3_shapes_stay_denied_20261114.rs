#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Regression guard (spec blastguard-comment-collapse-residual, R1 done
//! criterion "旧 R3", user ruling 2026-11-13): comment-bearing shapes that are
//! NOT Allow at HEAD must stay not-Allow after the e4523bfd fix.
//!
//! Each command runs a Python program whose `os.chmod('/etc/hosts',0)` call
//! sits on the line after a `#` comment line, reached through: an
//! unresolvable command word (`$PY -c`, `"$PY" -c`), `find -exec sh -c`, and
//! an ANSI-C `$'..'` program (newline spelled `\n`).
//!
//! Asserted: none is Allow, in the main session and in a subagent. Not
//! ignored: observed passing at main a473ba71 (2026-11-14).
//!
//! Detection evidence: with the e4523bfd RED shape
//! `myrunner python3 -c "import os # x<LF>os.chmod('/etc/hosts',0)"` added to
//! SHAPES, this test FAILED at main a473ba71 (that shape is Allow there), so
//! the checker does see an Allow; the shape was then removed again. Output is
//! recorded in the commit message.
//!
//! Written by the flow-8cca7ef5 orchestrator (not the e4523bfd implementer).
//!
//! DOES NOT PROVE: shapes beyond those listed; what the reason text says.

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
        "bg-r3c-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

/// Run the hook. Returns (decision, reason).
fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "comment-collapse-r3-20261114",
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

const SHAPES: &[&str] = &[
    "$PY -c \"import os # x\nos.chmod('/etc/hosts',0)\"",
    "\"$PY\" -c \"import os # x\nos.chmod('/etc/hosts',0)\"",
    "find . -exec sh -c 'python3 -c \"import os # x\nos.chmod(\\\"/etc/hosts\\\",0)\"' \\;",
    "python3 -c $'import os # x\\nos.chmod(\\'/etc/hosts\\',0)'",
];

#[test]
fn comment_before_system_harm_call_stays_not_allowed_on_r3_shapes() {
    let home = unique_home("r3");
    let mut allowed = Vec::new();
    for c in SHAPES {
        for subagent in [false, true] {
            let (d, r) = run_as(c, &home, subagent);
            if d == "allow" {
                allowed.push(format!("subagent={subagent} {c:?} -> {d} | {r}"));
            }
        }
    }
    assert!(
        allowed.is_empty(),
        "{} of {} (shape x mode) comment-bearing R3 runs were ALLOWED:\n{}",
        allowed.len(),
        SHAPES.len() * 2,
        allowed.join("\n")
    );
}
