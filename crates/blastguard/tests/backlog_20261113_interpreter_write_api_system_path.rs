#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (CRITICAL fail-open, observed 2026-11-13 at main f0854f26): an
//! interpreter one-liner that writes a SYSTEM-directory file through a write
//! API other than `open(.., 'w')` is Allowed.
//!
//! Shapes reported Allow by hand: python `pathlib.Path(..).write_text/
//! write_bytes`, node `fs.appendFileSync` / `fs.appendFile`. Additional shapes
//! probed here (they may or may not already be denied; the RED lists every
//! shape that is Allow at HEAD): perl 2-arg `open(FH, ">/etc/hosts")`, perl
//! `open my $fh, '>>', ...` without parens, python `shutil.copy` and
//! `os.replace` into a system path.
//!
//! Asserted: none of these is Allow, in main and subagent mode.
//! Control (GREEN at HEAD, not ignored): python `open('/etc/hosts','w')` is
//! denied.
//!
//! DOES NOT PROVE: non-system targets, other interpreters, or that every
//! write API is covered (only the shapes listed below).

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
        "bg-wapi-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

/// Run the hook. Returns (decision, reason).
fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "interp-write-api-20261113",
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

const WRITE_API_SHAPES: &[&str] = &[
    "python3 -c \"import pathlib; pathlib.Path('/etc/hosts').write_text('x')\"",
    "python3 -c \"import pathlib; pathlib.Path('/etc/hosts').write_bytes(b'x')\"",
    "node -e \"require('fs').appendFileSync('/etc/hosts','x')\"",
    "node -e \"require('fs').appendFile('/etc/hosts','x',()=>{})\"",
    "perl -e 'open(FH, \">/etc/hosts\"); print FH \"x\"'",
    "perl -e 'open my $fh, \">>\", \"/etc/hosts\"; print $fh \"x\"'",
    "python3 -c \"import shutil; shutil.copy('/tmp/x','/etc/hosts')\"",
    "python3 -c \"import os; os.replace('/tmp/x','/etc/hosts')\"",
];

// test-weakening-justified: new RED for an unfixed CRITICAL fail-open (backlog
// pending, 2026-11-13); the ignore is removed in the fixing commit.
#[test]
#[ignore = "RED: <backlog-pending> 2026-11-13 interpreter write APIs (pathlib/fs.append*/perl open/shutil/os.replace) on a system path are allowed — remove the ignore when fixed"]
fn interpreter_write_apis_on_system_path_are_not_allowed() {
    let home = unique_home("red");
    let mut failures = Vec::new();
    for c in WRITE_API_SHAPES {
        for subagent in [false, true] {
            let (d, r) = run_as(c, &home, subagent);
            if d == "allow" {
                failures.push(format!("subagent={subagent} {c:?} -> {d} | {r}"));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} interpreter write(s) to a system path were ALLOWED:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

// Control: python open(.., 'w') on the same path is denied.
#[test]
fn control_python_open_w_on_system_path_is_denied() {
    let home = unique_home("ctl");
    for subagent in [false, true] {
        let c = "python3 -c \"open('/etc/hosts','w').write('x')\"";
        let (d, r) = run_as(c, &home, subagent);
        assert_eq!(d, "deny", "subagent={subagent} {c:?} -> {d} | {r}");
    }
}
