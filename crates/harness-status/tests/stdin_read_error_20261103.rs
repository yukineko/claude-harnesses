#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (backlog 8004d80f, user ruling 2026-11-03): `harness-status
//! session-start` (src/main.rs ~:308) does not need the payload, so a stdin
//! READ ERROR must not suppress its health output. Today
//! `read_stdin_if_piped` panics on the error inside `run_hook`, the panic is
//! swallowed (exit 0) and the hook prints NOTHING — the missing-binary warning
//! the session exists to see is silently lost (an empty SessionStart stdout
//! reads as "all healthy"). Independent test author (CLAUDE.md §2(a)).
//!
//! Expected: exit 0 and the same `additionalContext` the control prints (a
//! readable payload in the same sandbox, whose settings.json registers a hook
//! binary that does not exist), and the read failure is named on stderr
//! without a panic trace.

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
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_harness-status"));
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

fn ghost_settings(sb: &Path) {
    let claude = sb.join("home").join(".claude");
    std::fs::create_dir_all(&claude).unwrap();
    std::fs::write(
        claude.join("settings.json"),
        serde_json::json!({"hooks": {"SessionStart": [{"hooks": [
            {"type": "command", "command": "/no/such/bin/ghost session-start"}
        ]}]}})
        .to_string(),
    )
    .unwrap();
}

fn context_of(o: &Out, case: &str) -> String {
    let v: serde_json::Value = serde_json::from_str(o.stdout.trim()).unwrap_or_else(|e| {
        panic!(
            "{case}: expected the health JSON on stdout ({e}); stdout={:?} stderr={:?}",
            o.stdout, o.stderr
        )
    });
    v["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap_or_else(|| panic!("{case}: no additionalContext in {v}"))
        .to_string()
}

fn control(sb: &Path) -> String {
    let p = serde_json::json!({"session_id": "s", "hook_event_name": "SessionStart",
        "cwd": sb.join("proj").to_string_lossy()})
    .to_string();
    let o = run(sb, &["session-start"], Stdin::Bytes(p.as_bytes()), &[]);
    assert_eq!(o.code, Some(0), "control: stderr={:?}", o.stderr);
    let ctx = context_of(&o, "control");
    assert!(ctx.contains("/no/such/bin/ghost"), "control: {ctx:?}");
    ctx
}

#[test]
fn harness_status_session_start_valid_payload_control() {
    let sb = sandbox();
    ghost_settings(sb.path());
    let _ = control(sb.path());
}

#[test]
fn harness_status_session_start_stdin_read_error_still_reports_health() {
    let sb = sandbox();
    ghost_settings(sb.path());
    let expected = control(sb.path());
    let o = run(
        sb.path(),
        &["session-start"],
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
    let ctx = context_of(&o, "stdin read error");
    assert!(
        ctx.contains(&expected),
        "the health report must be the control's, unchanged by the unreadable payload; \
         got {ctx:?}, control {expected:?}"
    );
    assert_read_failure_named(&o, "stdin read error");
}
