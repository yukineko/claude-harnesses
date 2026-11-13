#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (fail-open family of backlog c619b13f, observed 2026-11-13 at main
//! 4cf135cd, blastguard 0.2.485): residual newline-collapse ("re-join") sites
//! that the c619b13f faithful-inline-program fix did not reach.
//!
//! Each shape wraps a multi-line program whose system-harm call follows a
//! `#` comment line, reached through a different way of rebuilding the
//! command text. Only shapes observed Allow at HEAD are listed: unknown
//! wrapper tail, `env -S`, and a relinked operand in the same segment as the
//! program. (Probed and NOT Allow, so not listed: `$PY -c`, `"$PY" -c`,
//! `find -exec sh -c`, ANSI-C `$'..'`, `ln && python3 -c`, relink + `sh -c`.)
//!
//! Asserted (RED): none of the with-comment shapes is Allow, in the main
//! session and in a subagent. Control (not ignored): the same programs
//! without the comment are not Allow, so a RED failure is attributable to
//! the comment arrangement.
//!
//! DOES NOT PROVE: which internal stage loses the newline on each path (the
//! re-join sites in detect.rs / relink.rs / find_xargs.rs are suspected, not
//! observed); that shapes not listed here are safe.

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
        "bg-pcr-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

/// Run the hook. Returns (decision, reason).
fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "payload-collapse-residual-20261113",
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

/// (label, with-comment command, same command without the comment).
const SHAPES: &[(&str, &str, &str)] = &[
    (
        "unknown-wrapper",
        "myrunner python3 -c \"import os # x\nos.chmod('/etc/hosts',0)\"",
        "myrunner python3 -c \"import os\nos.chmod('/etc/hosts',0)\"",
    ),
    (
        "env-S",
        "env -S 'python3 -c \"import os # x\nos.chmod(\\\"/etc/hosts\\\",0)\"'",
        "env -S 'python3 -c \"import os\nos.chmod(\\\"/etc/hosts\\\",0)\"'",
    ),
    (
        "relink-operand-same-segment",
        "ln -sfn /tmp/x d && touch d/f python3 -c \"import os # x\nos.chmod('/etc/hosts',0)\"",
        "ln -sfn /tmp/x d && touch d/f python3 -c \"import os\nos.chmod('/etc/hosts',0)\"",
    ),
];

#[test]
fn comment_before_system_harm_call_is_not_allowed_on_residual_collapse_paths() {
    let home = unique_home("red");
    let mut failures = Vec::new();
    for (label, with_comment, _) in SHAPES {
        for subagent in [false, true] {
            let (d, r) = run_as(with_comment, &home, subagent);
            if d == "allow" {
                failures.push(format!(
                    "[{label}] subagent={subagent} {with_comment:?} -> {d} | {r}"
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} (shape x mode) runs whose system-harm call follows a comment were ALLOWED:\n{}",
        failures.len(),
        SHAPES.len() * 2,
        failures.join("\n")
    );
}

// Control: without the comment no shape is Allow.
#[test]
fn control_same_programs_without_comment_are_not_allowed() {
    let home = unique_home("ctl");
    let mut failures = Vec::new();
    for (label, _, without) in SHAPES {
        for subagent in [false, true] {
            let (d, r) = run_as(without, &home, subagent);
            if d == "allow" {
                failures.push(format!(
                    "[{label}] subagent={subagent} {without:?} -> {d} | {r}"
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} run(s) were ALLOWED even without the comment:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
