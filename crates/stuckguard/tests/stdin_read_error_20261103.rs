#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! User ruling 2026-11-03 (backlog 8004d80f): a stdin READ ERROR is a failure
//! ("cannot determine"), never an empty or partial `Ok` payload, and a
//! verdict-bearing caller must resolve it to the restrictive side.
//!
//! `stuckguard watch` (PostToolUse) can only advise, so its restrictive side
//! is the nudge: the same `hookSpecificOutput.additionalContext` channel it
//! uses for a detected loop and for an unreadable session history
//! (`undetermined_history_message`). A silent exit 0 reads downstream as "no
//! stuck loop" even though this tool call was never recorded or examined.
//! Expected: exit 0, a single-line PostToolUse `additionalContext` JSON that
//! names the stdin read failure (contains "stdin").
//!
//! Apparatus: the real binary (`CARGO_BIN_EXE_stuckguard`) is spawned with
//! fd 0 opened on a DIRECTORY (`read(2)` -> EISDIR on macOS and Linux), in an
//! isolated HOME/cwd. The control runs the same helper with a valid
//! single-event payload piped in and requires silence.
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
    let d = std::env::temp_dir().join(format!("sg-stdin-err-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// `None` -> stdin is a directory fd (read fails with EISDIR);
/// `Some(p)` -> stdin is a pipe carrying `p`.
fn run_watch(payload: Option<&str>) -> Run {
    let home = scratch_dir();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_stuckguard"));
    cmd.arg("watch")
        .current_dir(&home)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_STATE_HOME", home.join(".state"))
        .env_remove("HARNESS_SANDBOX_HOME")
        .env_remove("STUCKGUARD_DISABLE")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = match payload {
        None => {
            let dir_fd = std::fs::File::open(&home).expect("open temp dir as a file");
            cmd.stdin(Stdio::from(dir_fd));
            cmd.output().expect("run stuckguard watch")
        }
        Some(p) => {
            cmd.stdin(Stdio::piped());
            let mut child = cmd.spawn().expect("spawn stuckguard watch");
            child.stdin.take().unwrap().write_all(p.as_bytes()).unwrap();
            child.wait_with_output().expect("wait stuckguard watch")
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
fn stdin_read_error_nudges_naming_the_read_failure() {
    let run = run_watch(None);
    assert_ne!(
        run.code,
        Some(101),
        "a stdin read error must be handled, not surface as a raw panic (exit 101); stderr: {}",
        run.stderr
    );
    assert_eq!(
        run.code,
        Some(0),
        "PostToolUse advisory hook must exit 0; stderr: {}",
        run.stderr
    );
    let line = run.stdout.trim();
    assert!(
        !line.is_empty(),
        "stdin could not be READ, so this call was never recorded or examined: silence \
         reads as 'no stuck loop' (fail-open). stdout was empty; stderr: {}",
        run.stderr
    );
    assert_eq!(
        line.lines().count(),
        1,
        "nudge must be a single line, got: {line:?}"
    );
    let v: serde_json::Value = serde_json::from_str(line)
        .unwrap_or_else(|e| panic!("stdout must be valid JSON: {e}; stdout: {line:?}"));
    assert_eq!(
        v["hookSpecificOutput"]["hookEventName"], "PostToolUse",
        "nudge JSON: {v}"
    );
    let ctx = v["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap_or_else(|| panic!("additionalContext must be a string, got: {v}"));
    assert!(
        ctx.to_lowercase().contains("stdin"),
        "the nudge must name the stdin read failure (contain \"stdin\"): {ctx}"
    );
}

/// Control: same helper, a valid single non-repeating event -> silent, exit 0.
#[test]
fn control_valid_single_event_stays_silent() {
    let payload = r#"{"hook_event_name":"PostToolUse","session_id":"sg-stdin-ctl","tool_name":"Read","tool_input":{"file_path":"a.rs"}}"#;
    let run = run_watch(Some(payload));
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(
        run.stdout.trim(),
        "",
        "a single event trips no nudge; got: {}",
        run.stdout
    );
}
