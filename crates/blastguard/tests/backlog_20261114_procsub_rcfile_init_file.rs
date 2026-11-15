#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (observed 2026-11-14 by the orchestrator on target/debug/blastguard at 572cc1c3,
//! blastguard 0.2.535; also allow at base 0e726e66; reproduced by this independent
//! author), written by an independent author, not the implementer of the fix.
//!
//! Finding: an interactive bash sources its `--rcfile` / `--init-file` argument as a
//! program. When that argument is a process substitution whose output re-permissions
//! a system file (`/etc/hosts`), the program runs, but the verdict is Allow in main
//! and subagent mode. The procsub code consumes the option's value without judging it.
//!
//! Asserted (ignored RED): each harmful shape is DENY in main and subagent mode:
//! `--init-file <(..) -i`, `--rcfile <(..) -i`, `--rcfile <(..)` without `-i`,
//! `-i --rcfile <(..)`, and `--rcfile=<(..) -i`. All were observed allow/allow.
//! Control (not ignored, currently passes): the harmless twins with `echo hi`
//! (`--rcfile <(echo hi) -i`, `--init-file <(echo hi) -i`) are ALLOW in both modes.
//!
//! DOES NOT PROVE: other shells (`sh`, `zsh`, `ENV=`), other value-taking options
//! (`-o`, `-O`), other producers than echo, an rcfile given as a plain path or a
//! heredoc, or that the verdict in any non-procsub context is correct. The control
//! proves only that the fix must not flip these two harmless twins to Ask/Deny.

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
        "bg-procsubrc-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

/// Run the hook. Returns (decision, reason).
fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "procsub-rcfile-20261114",
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

const HARMFUL_SHAPES: &[(&str, &str)] = &[
    (
        "init-file-i",
        "bash --init-file <(echo 'chmod 777 /etc/hosts') -i",
    ),
    (
        "rcfile-i",
        "bash --rcfile <(echo 'chmod 777 /etc/hosts') -i",
    ),
    (
        "rcfile-no-i",
        "bash --rcfile <(echo 'chmod 777 /etc/hosts')",
    ),
    ("i-first", "bash -i --rcfile <(echo 'chmod 777 /etc/hosts')"),
    (
        "rcfile-eq",
        "bash --rcfile=<(echo 'chmod 777 /etc/hosts') -i",
    ),
];

const HARMLESS_SHAPES: &[(&str, &str)] = &[
    ("rcfile-hi", "bash --rcfile <(echo hi) -i"),
    ("init-file-hi", "bash --init-file <(echo hi) -i"),
];

#[test]
fn procsub_given_as_rcfile_or_init_file_with_system_harm_is_denied() {
    let failures = run_all(HARMFUL_SHAPES, "red", |d| d == "deny");
    assert!(
        failures.is_empty(),
        "{} of {} (shape x mode) runs were NOT Deny:\n{}",
        failures.len(),
        HARMFUL_SHAPES.len() * 2,
        failures.join("\n")
    );
}

#[test]
fn control_harmless_procsub_rcfile_twins_stay_allowed() {
    let failures = run_all(HARMLESS_SHAPES, "ctl-allow", |d| d == "allow");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
