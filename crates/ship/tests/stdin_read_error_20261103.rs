#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (backlog 8004d80f, user ruling 2026-11-03): `ship session-end`
//! (src/main.rs ~:155) must still print its unshipped-work reminder when the
//! SessionEnd payload cannot be READ. Today `read_stdin_if_piped` panics on
//! the error, nothing catches it before `run_hook`'s barrier, and the reminder
//! is silently lost (exit 0, empty stdout = "nothing to ship").
//! Independent test author (CLAUDE.md §2(a)).
//!
//! Expected: exit 0; stdout carries the same reminder the control prints for
//! the same repo (the hook's cwd is the repo, which is where Claude Code runs
//! it, so the payload's `cwd` is not needed to find it); stderr names the read
//! failure without a panic trace.

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
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_ship"));
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

/// `proj/` as a fresh git repo holding an uncommitted file (mirrors
/// tests/repro_session_end_suppressed_when_piped.rs).
fn dirty_repo(sb: &Path) {
    let p = sb.join("proj");
    let o = harness_core::git_env::scrub(&mut Command::new("git"))
        .args(["init", "-q"])
        .current_dir(&p)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .unwrap();
    assert!(o.status.success(), "git init: {o:?}");
    std::fs::write(p.join("dirty.txt"), "uncommitted").unwrap();
}

const UNCOMMITTED: &str = "未コミットの変更";

#[test]
fn ship_session_end_valid_payload_control() {
    let sb = sandbox();
    dirty_repo(sb.path());
    let p = serde_json::json!({"cwd": sb.path().join("proj").to_string_lossy(),
        "hook_event_name": "SessionEnd"})
    .to_string();
    let o = run(sb.path(), &["session-end"], Stdin::Bytes(p.as_bytes()), &[]);
    assert_eq!(o.code, Some(0), "control: stderr={:?}", o.stderr);
    assert!(
        o.stdout.contains(UNCOMMITTED),
        "control: the reminder names the uncommitted change; stdout={:?}",
        o.stdout
    );
}

#[test]
fn ship_session_end_stdin_read_error_still_reminds() {
    let sb = sandbox();
    dirty_repo(sb.path());
    let o = run(
        sb.path(),
        &["session-end"],
        Stdin::Dir(&sb.path().join("stdin-dir")),
        &[],
    );
    assert_eq!(
        o.code,
        Some(0),
        "stdout={:?} stderr={:?}",
        o.stdout,
        o.stderr
    );
    assert!(
        o.stdout.contains(UNCOMMITTED),
        "an unreadable SessionEnd payload must not suppress the unshipped-work reminder \
         for the cwd repo; stdout={:?} stderr={:?}",
        o.stdout,
        o.stderr
    );
    assert_read_failure_named(&o, "stdin read error");
}
