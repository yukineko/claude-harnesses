#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (backlog 8004d80f, user ruling 2026-11-03): `compass breadcrumb`
//! (Stop hook, src/main.rs ~:440) documents "ALWAYS exits 0 — a breadcrumb
//! must never block a turn". Today a stdin READ ERROR panics in `read_stdin`
//! with no barrier around it: exit 101. Independent test author (CLAUDE.md
//! §2(a)).
//!
//! Expected: exit 0 (the breadcrumb records nothing — it decides nothing, so
//! the honest outcome is "not recorded", said on stderr) and stderr names the
//! stdin read failure without a panic trace. Not a block: the breadcrumb is
//! not a gate, and its contract is exit 0.

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
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_compass"));
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
fn compass_breadcrumb_valid_payload_control() {
    let sb = sandbox();
    let p = serde_json::json!({"session_id": "s", "transcript_path": "",
        "cwd": sb.path().join("proj").to_string_lossy(), "hook_event_name": "Stop"})
    .to_string();
    let o = run(sb.path(), &["breadcrumb"], Stdin::Bytes(p.as_bytes()), &[]);
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
fn compass_breadcrumb_stdin_read_error_exits_zero_and_says_so() {
    let sb = sandbox();
    let o = run(
        sb.path(),
        &["breadcrumb"],
        Stdin::Dir(&sb.path().join("stdin-dir")),
        &[],
    );
    assert_eq!(
        o.code,
        Some(0),
        "breadcrumb ALWAYS exits 0 (its contract); stdout={:?} stderr={:?}",
        o.stdout,
        o.stderr
    );
    assert_read_failure_named(&o, "stdin read error");
}
