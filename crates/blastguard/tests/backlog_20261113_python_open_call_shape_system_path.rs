#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (fail-open, traced by an independent verifier then observed against the
//! real hook at fix/bg-python-open-write-modes 7c608bff, blastguard 0.2.483):
//! the python `open()` write-mode detection (detect/interpreter/open_mode.rs)
//! only recognises the literal call shape `open(`. Writes to a SYSTEM-directory
//! file that use a different call shape are Allowed in the main session and in
//! a subagent:
//!
//! 1. whitespace / newline between `open` and `(`
//! 2. aliased open: `from io import open as o`, `from os import open as sysopen`
//! 3. bare `open(path, 1)` after `from os import open` (os.open int flags)
//! 4. other writers of the same file: io.FileIO, zipfile.ZipFile (the getattr
//!    on builtins shape is already ask/deny and is not asserted)
//!
//! Asserted: none of the SHAPES (each Allow at HEAD) is Allow. Control (GREEN
//! at HEAD, not ignored): `open('/etc/hosts','a')` with no space is not Allow.
//!
//! DOES NOT PROVE: non-system targets, other interpreters, or that the
//! non-Allow shapes (Ask/Deny already) are denied for the right reason.

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
        "bg-pyshape-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

/// Run the hook. Returns (decision, reason).
fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "py-open-shape-20261113",
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

/// Shapes observed Allow at 7c608bff in both main and subagent mode.
/// Excluded: `open (p, mode='a')` (target is a variable, not a system path)
/// and `getattr(__builtins__,'open')(...)` (already ask/deny).
const SHAPES: &[&str] = &[
    "python3 -c \"open ('/etc/hosts','a').write('x')\"",
    "python3 -c \"open\n('/etc/hosts','a').write('x')\"",
    "python3 -c \"open  ('/etc/hosts','a').write('x')\"",
    "python3 -c \"from io import open as o; o('/etc/hosts','a').write('x')\"",
    "python3 -c \"from os import open as sysopen; sysopen('/etc/hosts',1)\"",
    "python3 -c \"from os import open; open('/etc/hosts', 1)\"",
    "python3 -c \"import io; io.FileIO('/etc/hosts','a')\"",
    "python3 -c \"import zipfile; zipfile.ZipFile('/etc/hosts','a')\"",
];

// test-weakening-justified: new RED for an unfixed fail-open; the ignore is
// removed in the fixing commit.
#[test]
fn python_open_call_shapes_on_system_path_are_not_allowed() {
    let home = unique_home("red");
    let mut failures = Vec::new();
    for c in SHAPES {
        for subagent in [false, true] {
            let (d, r) = run_as(c, &home, subagent);
            if d == "allow" {
                failures.push(format!("subagent={subagent} {c:?} -> {d} | {r}"));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} python open-shape write(s) to a system path were ALLOWED:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

// Control: the plain `open('/etc/hosts','a')` shape is not allowed.
#[test]
fn control_plain_open_append_on_system_path_is_not_allowed() {
    let home = unique_home("ctl");
    for subagent in [false, true] {
        let c = "python3 -c \"open('/etc/hosts','a').write('x')\"";
        let (d, r) = run_as(c, &home, subagent);
        assert_ne!(d, "allow", "subagent={subagent} {c:?} -> {d} | {r}");
    }
}
