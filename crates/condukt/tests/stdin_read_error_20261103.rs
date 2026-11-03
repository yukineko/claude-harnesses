#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! backlog 8004d80f (user ruling 2026-11-03) — a stdin READ ERROR is
//! "cannot determine", never an empty or partial `Ok` payload.
//!
//! `condukt`'s private `read_stdin` does `let _ = read_to_string(..)`, so an
//! I/O failure is indistinguishable from empty stdin. These tests open stdin on
//! a DIRECTORY (read(2) fails with EISDIR) and require every consumer to name
//! the read failure instead of misattributing it to "empty" / "not JSON".
//!
//! Expected (per test) once fixed:
//!  1. editgate (PreToolUse-style verdict hook): stdout carries one line
//!     `{"decision":"block","reason":"...stdin...read..."}` (the same shape as
//!     its config-undetermined block); exit 0 (verdict channel is stdout).
//!  2. subagent-stop (no verdict channel; exit 0 by contract): writes nothing,
//!     and stderr names the stdin READ failure, not "stdin was empty or not a
//!     parseable hook payload".
//!  3. `validate` with no --file: exit non-zero, stderr names the stdin read
//!     failure, not "parsing decomposition JSON".
//!  4. session-heartbeat: exit non-zero, stderr names the stdin read failure,
//!     not "hook payload is not JSON".
//! Controls (stdin = /dev/null, i.e. genuinely empty) pin today's behavior so
//! the fix cannot regress the empty-stdin contract.

use std::fs::File;
use std::path::Path;
use std::process::{Command, Output, Stdio};

enum In {
    /// A directory opened as stdin: read(2) -> EISDIR.
    Dir,
    /// Genuinely empty stdin.
    Null,
}

fn condukt(home: &Path, cwd: &Path, args: &[&str], stdin: In) -> Output {
    let stdio = match stdin {
        In::Dir => Stdio::from(File::open(cwd).expect("open directory as stdin")),
        In::Null => Stdio::null(),
    };
    harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_condukt")))
        .args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .env("CONDUKT_DEFAULT_BRANCH", "main")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove("CONDUKT_DISABLE")
        .stdin(stdio)
        .output()
        .expect("spawn condukt")
}

fn names_read_failure(stderr: &str) -> bool {
    let s = stderr.to_lowercase();
    s.contains("stdin") && (s.contains("read") || s.contains("is a directory"))
}

#[test]
fn editgate_blocks_on_stdin_read_error() {
    let tmp = tempfile::tempdir().unwrap();
    let out = condukt(tmp.path(), tmp.path(), &["editgate"], In::Dir);
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let line = stdout.lines().find(|l| !l.trim().is_empty()).unwrap_or_else(|| {
        panic!(
            "editgate was SILENT (= allow) on a stdin read error; expected a block line. \
             stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        )
    });
    let v: serde_json::Value = serde_json::from_str(line).expect("block line is JSON");
    assert_eq!(v["decision"], "block", "stdout: {stdout}");
    let reason = v["reason"].as_str().unwrap_or("").to_lowercase();
    assert!(
        reason.contains("stdin") && reason.contains("read"),
        "block reason must name the stdin read failure, got: {reason}"
    );
}

#[test]
fn editgate_control_empty_stdin_stays_silent() {
    let tmp = tempfile::tempdir().unwrap();
    let out = condukt(tmp.path(), tmp.path(), &["editgate"], In::Null);
    assert!(out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stdout).trim().is_empty(),
        "empty stdin must keep printing nothing"
    );
}

#[test]
fn subagent_stop_names_stdin_read_failure() {
    let tmp = tempfile::tempdir().unwrap();
    let out = condukt(tmp.path(), tmp.path(), &["subagent-stop"], In::Dir);
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(out.status.success(), "hook exit-0 contract; stderr: {stderr}");
    assert!(
        String::from_utf8_lossy(&out.stdout).trim().is_empty(),
        "no verdict channel"
    );
    assert!(
        names_read_failure(&stderr) && !stderr.contains("was empty"),
        "stderr must name the stdin READ failure, not call it empty/unparseable; got: {stderr}"
    );
}

#[test]
fn subagent_stop_control_empty_stdin_says_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let out = condukt(tmp.path(), tmp.path(), &["subagent-stop"], In::Null);
    assert!(out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("no progress timestamp written"),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn validate_stdin_read_error_is_reported_as_read_error() {
    let tmp = tempfile::tempdir().unwrap();
    let out = condukt(tmp.path(), tmp.path(), &["validate"], In::Dir);
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(!out.status.success(), "must exit non-zero");
    assert!(
        names_read_failure(&stderr) && !stderr.contains("parsing decomposition JSON"),
        "stderr must name the stdin read failure, not a parse of empty input; got: {stderr}"
    );
}

#[test]
fn validate_control_empty_stdin_is_a_parse_error() {
    let tmp = tempfile::tempdir().unwrap();
    let out = condukt(tmp.path(), tmp.path(), &["validate"], In::Null);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("parsing decomposition JSON"),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn session_heartbeat_names_stdin_read_failure() {
    let tmp = tempfile::tempdir().unwrap();
    let out = condukt(tmp.path(), tmp.path(), &["session-heartbeat"], In::Dir);
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(!out.status.success(), "must exit non-zero; stderr: {stderr}");
    assert!(
        names_read_failure(&stderr) && !stderr.contains("not JSON"),
        "stderr must name the stdin read failure, not 'not JSON'; got: {stderr}"
    );
}

#[test]
fn session_heartbeat_control_empty_stdin_is_not_json() {
    let tmp = tempfile::tempdir().unwrap();
    let out = condukt(tmp.path(), tmp.path(), &["session-heartbeat"], In::Null);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("not JSON"));
}
