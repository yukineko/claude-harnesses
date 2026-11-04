#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! User ruling 2026-11-03 (backlog 8004d80f): a stdin READ ERROR is a failure
//! ("cannot determine"), never an empty or partial `Ok` payload, and a
//! verdict-bearing caller must resolve it to the restrictive side.
//!
//! `fetchguard scan` is verdict-bearing: its silence is read downstream as
//! "scanned, no planted instruction found" (see `tests/unreadable_payload.rs`).
//! A payload that could not even be READ was never scanned, exactly like the
//! unparseable payload case, so it must produce the same fail-closed
//! `additionalContext` warning, and that warning must name the stdin read
//! failure (so the two causes are distinguishable to a reader).
//!
//! Apparatus: the real binary (`CARGO_BIN_EXE_fetchguard`) is spawned with
//! fd 0 opened on a DIRECTORY. `read(2)` on a directory fd fails with EISDIR
//! (macOS and Linux), so `read_to_end` returns `Err` on the first read. The
//! control test runs the very same spawn helper with a valid benign payload
//! piped in and requires silence, so the helper is shown able to tell the two
//! outcomes apart.
//!
//! Written by a disinterested author (CLAUDE.md section 2(a)).

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

static N: AtomicU32 = AtomicU32::new(0);

struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

fn scratch_dir() -> PathBuf {
    let n = N.fetch_add(1, Ordering::SeqCst);
    let d = std::env::temp_dir().join(format!("fg-stdin-err-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// `None` -> stdin is a directory fd (read fails with EISDIR);
/// `Some(p)` -> stdin is a pipe carrying `p`.
fn run_scan(payload: Option<&str>) -> Run {
    let home = scratch_dir();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_fetchguard"));
    cmd.arg("scan")
        .current_dir(&home)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_STATE_HOME", home.join(".state"))
        .env_remove("HARNESS_SANDBOX_HOME")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = match payload {
        None => {
            let dir_fd = std::fs::File::open(&home).expect("open temp dir as a file");
            cmd.stdin(Stdio::from(dir_fd));
            cmd.output().expect("run fetchguard scan")
        }
        Some(p) => {
            cmd.stdin(Stdio::piped());
            let mut child = cmd.spawn().expect("spawn fetchguard scan");
            child.stdin.take().unwrap().write_all(p.as_bytes()).unwrap();
            child.wait_with_output().expect("wait fetchguard scan")
        }
    };
    let _ = std::fs::remove_dir_all(&home);
    Run {
        code: out.status.code(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

#[test]
fn stdin_read_error_emits_fail_closed_warning_naming_the_read_failure() {
    let run = run_scan(None);
    assert_ne!(
        run.code,
        Some(101),
        "a stdin read error must be handled, not surface as a raw panic (exit 101); stderr: {}",
        run.stderr
    );
    assert_eq!(
        run.code,
        Some(0),
        "PostToolUse warn-only hook communicates via stdout JSON + exit 0; stderr: {}",
        run.stderr
    );
    let line = run.stdout.trim();
    assert!(
        !line.is_empty(),
        "stdin could not be READ, so nothing was scanned: silence here reads as \
         'no injection found' (fail-open). stdout was empty; stderr: {}",
        run.stderr
    );
    assert_eq!(
        line.lines().count(),
        1,
        "warning must be a single line, got: {line:?}"
    );
    let v: serde_json::Value = serde_json::from_str(line)
        .unwrap_or_else(|e| panic!("stdout must be valid JSON: {e}; stdout: {line:?}"));
    assert_eq!(
        v["hookSpecificOutput"]["hookEventName"], "PostToolUse",
        "warning JSON: {v}"
    );
    let ctx = v["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap_or_else(|| panic!("additionalContext must be a string, got: {v}"));
    assert!(
        ctx.contains("UNTRUSTED DATA"),
        "must use the crate's fail-closed vocabulary (\"UNTRUSTED DATA\"): {ctx}"
    );
    assert!(
        ctx.to_lowercase().contains("stdin"),
        "the warning must name the stdin read failure (contain \"stdin\"), so it is \
         distinguishable from the unparseable-payload warning: {ctx}"
    );
}

/// Control: same helper, valid benign payload -> silent, exit 0.
#[test]
fn control_valid_benign_payload_stays_silent() {
    let payload = serde_json::json!({
        "tool_name": "WebFetch",
        "tool_response": "The weather is sunny.",
    })
    .to_string();
    let run = run_scan(Some(&payload));
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(
        run.stdout.trim(),
        "",
        "a valid benign payload must stay silent; got: {}",
        run.stdout
    );
}
