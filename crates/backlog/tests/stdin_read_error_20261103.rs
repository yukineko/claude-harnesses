#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (backlog 8004d80f, user ruling 2026-11-03): `backlog session-start`
//! (src/main.rs ~:1643) already fails VISIBLY on a malformed payload (exit 1,
//! "backlog context was NOT injected" — tests/repro_session_start_malformed_stdin.rs).
//! A stdin READ ERROR must take the same visible-failure path, naming the read
//! failure. Today it panics in `read_stdin` before any handling: exit 101 and a
//! panic trace. Independent test author (CLAUDE.md §2(a)).
//!
//! Expected: exit 1 (not 101, not 0), stderr says the context was NOT injected
//! and names the stdin read failure (with its OS error), no panic trace.

#![cfg(unix)]

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

struct Out {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

enum Stdin<'a> {
    /// A directory opened read-only: `read(2)` fails with EISDIR.
    Dir(&'a Path),
    /// A pipe carrying these bytes, then EOF.
    Bytes(&'a [u8]),
}

/// `home/` (HOME), `proj/` (cwd), `stdin-dir/` (what fd 0 is opened on).
fn sandbox() -> tempfile::TempDir {
    let t = tempfile::TempDir::new().expect("tempdir");
    for d in ["home", "proj", "stdin-dir"] {
        std::fs::create_dir_all(t.path().join(d)).expect("mkdir");
    }
    t
}

/// Run the real binary with a CLEARED environment (no `*_DISABLE`, `GIT_*`,
/// `XDG_*` or session id leaking in from the test runner): only PATH, HOME,
/// a null global/system git config, and `extra`.
fn run(sb: &Path, args: &[&str], stdin: Stdin<'_>, extra: &[(&str, &Path)]) -> Out {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_backlog"));
    cmd.args(args)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", sb.join("home"))
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .current_dir(sb.join("proj"))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in extra {
        cmd.env(k, v);
    }
    let out = match stdin {
        Stdin::Dir(d) => {
            cmd.stdin(Stdio::from(
                std::fs::File::open(d).expect("open dir for stdin"),
            ));
            cmd.output().expect("binary runs")
        }
        Stdin::Bytes(b) => {
            cmd.stdin(Stdio::piped());
            let mut child = cmd.spawn().expect("binary spawns");
            let _ = child.stdin.take().expect("stdin pipe").write_all(b);
            child.wait_with_output().expect("binary runs")
        }
    };
    Out {
        code: out.status.code(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

/// The read failure is HANDLED, not crashed through: stderr names it (the
/// word "stdin" plus the OS error text for EISDIR) and carries no panic trace.
/// A panic — uncaught (exit 101) or swallowed by `run_hook`'s barrier — is
/// indistinguishable from any other crash of the hook, so it does not count
/// as surfacing the read failure.
fn assert_read_failure_named(o: &Out, case: &str) {
    assert!(
        !o.stderr.contains("panicked") && !o.stderr.contains("[harness hook panic]"),
        "{case}: the stdin read error must be handled, not surfaced as a panic. stderr={:?}",
        o.stderr
    );
    assert!(
        o.stderr.contains("stdin") && o.stderr.contains("Is a directory"),
        "{case}: stderr must name the stdin read failure and its OS error. stderr={:?}",
        o.stderr
    );
}

#[test]
fn backlog_session_start_valid_payload_control() {
    let sb = sandbox();
    let p = serde_json::json!({"session_id": "s", "hook_event_name": "SessionStart",
        "cwd": sb.path().join("proj").to_string_lossy()})
    .to_string();
    let o = run(
        sb.path(),
        &["session-start"],
        Stdin::Bytes(p.as_bytes()),
        &[],
    );
    assert_eq!(
        o.code,
        Some(0),
        "control: stdout={:?} stderr={:?}",
        o.stdout,
        o.stderr
    );
    let v: serde_json::Value = serde_json::from_str(o.stdout.trim()).expect("control JSON");
    assert_eq!(
        v["hookSpecificOutput"]["hookEventName"], "SessionStart",
        "control: {v}"
    );
}

#[test]
fn backlog_session_start_stdin_read_error_fails_visibly() {
    let sb = sandbox();
    let o = run(
        sb.path(),
        &["session-start"],
        Stdin::Dir(&sb.path().join("stdin-dir")),
        &[],
    );
    assert_eq!(
        o.code,
        Some(1),
        "a stdin read error must be the visible failure exit 1 (101 = uncaught panic); \
         stdout={:?} stderr={:?}",
        o.stdout,
        o.stderr
    );
    assert!(
        o.stderr.contains("NOT injected"),
        "stderr must say the backlog context was NOT injected; stderr={:?}",
        o.stderr
    );
    assert_read_failure_named(&o, "stdin read error");
}
