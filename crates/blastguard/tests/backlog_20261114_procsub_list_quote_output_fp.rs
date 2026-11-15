#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (observed 2026-11-14 by the t2 implementer at aa47d220, blastguard 0.2.533;
//! reproduced by this independent author with target/debug/blastguard), written by an
//! independent author, not the implementer of the fix.
//!
//! False positive: `bash <(echo "it's"; echo hi)` and `bash < <(echo "it's"; echo hi)`
//! feed a shell the output of two literal echoes. That output is statically provable
//! and harmless, but the verdict is Ask (Deny in subagent mode) only because the
//! combined output contains a `'`. Both were Allow at base 0e726e66.
//!
//! Asserted: the two harmless shapes are ALLOW in main and subagent mode (ignored RED).
//! Control (not ignored, currently passes): the same quote-bearing list whose second
//! echo prints a system-file re-permission (`bash <(echo "don't"; echo '<chmod 777 on
//! /etc/hosts>')`) is DENY in both modes; system harm stays Deny.
//!
//! DOES NOT PROVE: other quote characters (`"` inside single quotes, backticks),
//! lists with other separators or non-echo members, other producers, other shells
//! (`source`, `sh`), or that quote-bearing output is judged correctly in non-procsub
//! contexts.

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
        "bg-procsubq-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

/// Run the hook. Returns (decision, reason).
fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "procsub-quote-20261114",
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

/// Run every shape in both modes; return the (shape x mode) runs whose decision
/// does not satisfy `ok`.
fn run_all(shapes: &[(&str, &str)], tag: &str, ok: impl Fn(&str) -> bool) -> Vec<String> {
    let home = unique_home(tag);
    let mut failures = Vec::new();
    for (label, c) in shapes {
        for subagent in [false, true] {
            let (d, r) = run_as(c, &home, subagent);
            println!("[{label}] subagent={subagent} {c:?} -> {d} | {r}");
            if !ok(&d) {
                failures.push(format!("[{label}] subagent={subagent} {c:?} -> {d} | {r}"));
            }
        }
    }
    failures
}

const HARMLESS_SHAPES: &[(&str, &str)] = &[
    ("list-operand", "bash <(echo \"it's\"; echo hi)"),
    ("list-stdin", "bash < <(echo \"it's\"; echo hi)"),
];

#[test]
fn harmless_procsub_list_with_quote_in_output_is_allowed() {
    let failures = run_all(HARMLESS_SHAPES, "red", |d| d == "allow");
    assert!(
        failures.is_empty(),
        "{} of {} (shape x mode) runs were NOT Allow:\n{}",
        failures.len(),
        HARMLESS_SHAPES.len() * 2,
        failures.join("\n")
    );
}

#[test]
fn control_quote_bearing_procsub_list_with_system_harm_is_denied() {
    let failures = run_all(
        &[(
            "harm-operand",
            "bash <(echo \"don't\"; echo 'chmod 777 /etc/hosts')",
        )],
        "ctl-deny",
        |d| d == "deny",
    );
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
