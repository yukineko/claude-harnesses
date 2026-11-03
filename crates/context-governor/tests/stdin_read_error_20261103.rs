#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (backlog 8004d80f, user ruling 2026-11-03): the `context-governor`
//! hook binary (src/bin/context-governor.rs ~:57) must surface a stdin READ
//! ERROR as such. Independent test author (CLAUDE.md §2(a)).
//!
//! Expected: exit 0 (never blocks — with the payload unread the event is
//! unknown, and the only blocking event, PreCompact, cannot be identified; an
//! exit 2 on any other event would block a prompt/tool, which is not this
//! hook's job), and stderr names the stdin read failure with its OS error.
//!
//! Judged beyond the literal brief: today the binary ALREADY exits 0 and the
//! panic text "stdin read failed: Is a directory" reaches stderr — but only as
//! a panic trace swallowed by `run_hook`'s barrier ("[harness hook panic]").
//! That is the generic crash path, indistinguishable from any other bug in the
//! hook, so the test additionally requires the error to be HANDLED (no panic
//! trace). That extra clause is the only thing that is RED here today.

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
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_context-governor"));
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

fn state(sb: &Path) -> std::path::PathBuf {
    let d = sb.join("home").join("cg-state");
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn context_governor_valid_payload_control() {
    let sb = sandbox();
    let st = state(sb.path());
    let p = serde_json::json!({"session_id": "s", "hook_event_name": "SessionStart",
        "cwd": sb.path().join("proj").to_string_lossy()})
    .to_string();
    let o = run(
        sb.path(),
        &[],
        Stdin::Bytes(p.as_bytes()),
        &[("CONTEXT_GOVERNOR_STATE_DIR", &st)],
    );
    assert_eq!(
        o.code,
        Some(0),
        "control: stdout={:?} stderr={:?}",
        o.stdout,
        o.stderr
    );
    assert!(
        !o.stderr.contains("stdin"),
        "control: stderr={:?}",
        o.stderr
    );
}

#[test]
fn context_governor_stdin_read_error_is_named_and_never_blocks() {
    let sb = sandbox();
    let st = state(sb.path());
    let o = run(
        sb.path(),
        &[],
        Stdin::Dir(&sb.path().join("stdin-dir")),
        &[("CONTEXT_GOVERNOR_STATE_DIR", &st)],
    );
    assert_eq!(
        o.code,
        Some(0),
        "never blocks; stdout={:?} stderr={:?}",
        o.stdout,
        o.stderr
    );
    assert_read_failure_named(&o, "stdin read error");
}
