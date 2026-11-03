#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (backlog 8004d80f, user ruling 2026-11-03): a stdin READ ERROR at the
//! `webqa gate` (src/main.rs ~:95, private read with `let _ =`: today the error becomes an empty buffer = manual-CLI mode, which reports "not configured" and allows the stop) Stop gate is "cannot determine" and must resolve to a fail-closed
//! `{"decision":"block"}` whose reason contains "stdin read failed" — the
//! shape `harness_core::gate::run::read_stop_payload_guarded` prints.
//!
//! Written by an independent test author (CLAUDE.md §2(a)); process-level: the
//! real binary is spawned with fd 0 opened on a DIRECTORY, so `read(2)` fails
//! with EISDIR. Today the payload is read BEFORE `run_guarded`, so the error
//! either panics outside any barrier (exit 101: Claude Code treats it as a
//! non-blocking error and the stop proceeds) or is discarded into an empty
//! payload (manual-CLI mode). Both are permissive outcomes for an unread payload.
//!
//! Pinned for the chain-first stop with `CLAUDE_CODE_SESSION_ID` set and with
//! it unset (no usable panic-streak marker → block, never the bounded allow).
//! The control shows the same sandbox with a readable payload, so the only
//! difference the read-error tests exercise is fd 0.
//!
//! Not pinned: the bounded allow on a consecutive read-error stop (that policy
//! is harness-core's and is unit-tested there).

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

struct Out {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

/// What the hook's fd 0 is wired to.
enum Stdin<'a> {
    /// A directory opened read-only: every `read(2)` on it fails with EISDIR,
    /// so the hook's stdin read returns an `io::Error` (not EOF, not bytes).
    Dir(&'a Path),
    /// A pipe carrying these bytes, then EOF.
    Bytes(&'a [u8]),
}

/// A throwaway sandbox: `home/` (HOME, so every state dir, panic-streak marker
/// and config lookup lands here), `proj/` (the hook's cwd; not a git repo) and
/// `stdin-dir/` (the directory fd 0 is opened on for the read-error case).
fn sandbox() -> tempfile::TempDir {
    let t = tempfile::TempDir::new().expect("tempdir");
    for d in ["home", "proj", "stdin-dir"] {
        std::fs::create_dir_all(t.path().join(d)).expect("mkdir");
    }
    t
}

/// Run the real hook binary with a CLEARED environment (no `*_DISABLE`, no
/// `GIT_*`, no `XDG_*`, no autonomy/trust overrides leak in from the test
/// runner) — only PATH, HOME and, when given, `CLAUDE_CODE_SESSION_ID`.
fn run(sb: &Path, stdin: Stdin<'_>, session: Option<&str>) -> Out {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_webqa"));
    cmd.args(["gate"])
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", sb.join("home"))
        .current_dir(sb.join("proj"))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(s) = session {
        cmd.env("CLAUDE_CODE_SESSION_ID", s);
    }
    let out = match stdin {
        Stdin::Dir(d) => {
            cmd.stdin(Stdio::from(
                std::fs::File::open(d).expect("open dir for stdin"),
            ));
            cmd.output().expect("hook binary runs")
        }
        Stdin::Bytes(b) => {
            cmd.stdin(Stdio::piped());
            let mut child = cmd.spawn().expect("hook binary spawns");
            // The hook may exit before draining stdin; EPIPE here is not the
            // behaviour under test.
            let _ = child.stdin.take().expect("stdin pipe").write_all(b);
            child.wait_with_output().expect("hook binary runs")
        }
    };
    Out {
        code: out.status.code(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

fn valid_stop_payload(sb: &Path) -> Vec<u8> {
    serde_json::json!({
        "session_id": "stdin-read-error-control",
        "transcript_path": "",
        "cwd": sb.join("proj").to_string_lossy(),
        "hook_event_name": "Stop",
        "stop_hook_active": false,
    })
    .to_string()
    .into_bytes()
}

/// The expected outcome for a stdin read error at a Stop gate: exit 0 (the
/// hook protocol's channel is the `decision` field, not the exit code) and a
/// `{"decision":"block"}` JSON line whose reason names the read failure.
/// Never an allow (no/empty output), never manual-CLI mode (stderr + exit 1),
/// never an uncaught panic (exit 101, which Claude Code reads as a
/// non-blocking error = the stop proceeds).
fn assert_stdin_failure_block(o: &Out, case: &str) {
    assert_eq!(
        o.code,
        Some(0),
        "{case}: a stdin read error at the webqa Stop gate must exit 0 with a block \
         decision (exit 101 = uncaught panic, exit 1 = manual-CLI mode; both let the stop \
         through). stdout={:?} stderr={:?}",
        o.stdout,
        o.stderr
    );
    let last = o
        .stdout
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or_else(|| {
            panic!(
                "{case}: no stdout at all — an empty Stop-hook stdout is an ALLOW, but the \
                 payload was never read. stderr={:?}",
                o.stderr
            )
        });
    let v: serde_json::Value = serde_json::from_str(last).unwrap_or_else(|e| {
        panic!(
            "{case}: last stdout line is not JSON ({e}): {last:?}; stderr={:?}",
            o.stderr
        )
    });
    assert_eq!(
        v["decision"], "block",
        "{case}: the decision must be block, got {v}; stderr={:?}",
        o.stderr
    );
    let reason = v["reason"].as_str().unwrap_or_default();
    assert!(
        reason.contains("stdin read failed"),
        "{case}: the block reason must name the stdin read failure, got {reason:?}"
    );
}

#[test]
fn webqa_stop_stdin_read_error_blocks_with_session_set() {
    let sb = sandbox();
    let o = run(
        sb.path(),
        Stdin::Dir(&sb.path().join("stdin-dir")),
        Some("stdin-read-error-sess"),
    );
    assert_stdin_failure_block(&o, "CLAUDE_CODE_SESSION_ID set (chain-first stop)");
}

#[test]
fn webqa_stop_stdin_read_error_blocks_with_session_unset() {
    let sb = sandbox();
    let o = run(sb.path(), Stdin::Dir(&sb.path().join("stdin-dir")), None);
    assert_stdin_failure_block(&o, "CLAUDE_CODE_SESSION_ID unset");
}

/// Control: the same sandbox with a readable, valid Stop payload (cwd = a
/// non-git, unconfigured dir) is the gate's normal outcome there: exit 0 and
/// an empty stdout (allow). The read-error tests differ from this only in fd 0.
#[test]
fn webqa_stop_valid_payload_control() {
    let sb = sandbox();
    let o = run(
        sb.path(),
        Stdin::Bytes(&valid_stop_payload(sb.path())),
        Some("stdin-read-error-sess"),
    );
    assert_eq!(
        o.code,
        Some(0),
        "control: stdout={:?} stderr={:?}",
        o.stdout,
        o.stderr
    );
    assert!(
        o.stdout.trim().is_empty(),
        "control: a readable payload in an unconfigured non-git dir is allowed (no output); \
         stdout={:?} stderr={:?}",
        o.stdout,
        o.stderr
    );
}
