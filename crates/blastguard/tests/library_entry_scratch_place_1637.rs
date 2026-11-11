#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro for GitHub issue #1637 (the issue body is empty; this is inferred
//! from the title and `crates/blastguard/src/detect.rs`): library callers
//! (condukt `gate_exec.rs`/`verify.rs`, specguard's forge, daily's task runner)
//! go through `blastguard::detect::detect`, which uses `SafeRoots::none()`, and
//! `detect/scratch_place.rs` documents that with no location model the
//! scratch-place rule "does not apply". So a `git clone` into a place that is
//! outside the managed places (never cleaned up) is `Ask` from the hook binary
//! but `Allow` from the library entry — the unattended callers get the MORE
//! permissive verdict for the same command.
//!
//! The control proves the hook side asks (so the fixture is sound); the RED
//! test asserts the library entry does not answer `Allow` for the same command.

use blastguard::detect::detect;
use blastguard::model::Decision;
use std::io::Write;
use std::process::{Command, Stdio};

const OUTSIDE: &str = "/var/blastguard-1637-outside/clone";

fn command() -> String {
    format!("git clone https://example.invalid/r.git {OUTSIDE}")
}

fn hook_output() -> String {
    let base = std::env::temp_dir().join(format!("bg-1637-{}", std::process::id()));
    let home = base.join("home");
    let cwd = base.join("proj");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&cwd).unwrap();
    let cwd = std::fs::canonicalize(&cwd).unwrap();
    let payload = serde_json::json!({
        "session_id": "1637",
        "cwd": cwd.display().to_string(),
        "tool_name": "Bash",
        "tool_input": { "command": command() },
    })
    .to_string();
    let mut child = Command::new(env!("CARGO_BIN_EXE_blastguard"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("CLAUDE_PROJECT_DIR", &cwd)
        .env("BLASTGUARD_APPROVALS_DIR", base.join("store"))
        .env("HOME", &home)
        .env_remove("BLASTGUARD_ASK")
        .env_remove("TMPDIR")
        .current_dir(&base)
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let _ = std::fs::remove_dir_all(&base);
    assert_eq!(out.status.code(), Some(0));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Control (passes today): the hook binary does not allow the clone.
#[test]
fn control_hook_side_asks_for_clone_outside_managed_places() {
    let out = hook_output();
    assert!(
        out.contains("outside the managed places"),
        "hook side must Ask for the scratch-place reason; got {out:?}"
    );
}

#[test]
#[ignore = "#1637 RED: library entry detect() bypasses the scratch-place rule (SafeRoots::none)"]
fn library_entry_does_not_allow_clone_outside_managed_places() {
    let input = serde_json::json!({ "command": command() });
    let d = detect("Bash", Some(&input));
    assert!(
        !matches!(d, Decision::Allow),
        "library detect() answered Allow for `{}` while the hook binary Asks (scratch-place rule bypassed)",
        command()
    );
}
