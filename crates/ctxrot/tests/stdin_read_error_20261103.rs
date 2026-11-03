#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (backlog 8004d80f, user ruling 2026-11-03): a stdin READ ERROR is a
//! failure ("cannot determine"), never an empty/partial payload. Written by an
//! independent test author (CLAUDE.md §2(a)). Process-level: the real `ctxrot`
//! binary is spawned with fd 0 opened on a DIRECTORY, so `read(2)` fails with
//! EISDIR. Four entry points are pinned:
//!
//! * `ctxrot stop` (Stop gate, src/main.rs ~:952) — reads before `run_guarded`,
//!   so today the read panic is uncaught (exit 101 = non-blocking error = the
//!   stop proceeds). Expected: exit 0 + `{"decision":"block"}` whose reason
//!   contains "stdin read failed" (`read_stop_payload_guarded`'s shape), with
//!   `CLAUDE_CODE_SESSION_ID` set (chain-first) and unset.
//! * `ctxrot toolguard` (PostToolUse, ~:402) — inside `run_hook`, so today the
//!   read panic is swallowed: exit 0, NO stdout, and the (possibly huge) tool
//!   output passes through unexamined and unannounced. Expected: exit 0 and one
//!   JSON line `hookSpecificOutput.hookEventName == "PostToolUse"` whose
//!   `additionalContext` mentions "stdin" (the output's size is unknown).
//! * `ctxrot statusline` (~:794) — today an uncaught panic (exit 101): the
//!   status bar renders blank, which reads as "nothing to report". Expected:
//!   exit 0 and exactly the explicit unknown band `usage::unknown_line`
//!   prints (the same line a non-empty unparseable stdin produces today —
//!   that is the control, so the assertion is pinned to the real renderer
//!   rather than a hand-copied string).
//! * `ctxrot guard` (UserPromptSubmit, ~:327) — inside `run_hook`: today exit
//!   0 and no stdout, indistinguishable from "nothing to inject". Expected:
//!   exit 0 and one JSON line `hookSpecificOutput.hookEventName ==
//!   "UserPromptSubmit"` whose `additionalContext` mentions "stdin".
//!
//! Each has a control with a readable payload in the same sandbox, so the only
//! difference the read-error tests exercise is fd 0.

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

/// `home/` (HOME: config, note store, panic-streak markers), `proj/` (cwd,
/// not a git repo), `stdin-dir/` (what fd 0 is opened on).
fn sandbox() -> tempfile::TempDir {
    let t = tempfile::TempDir::new().expect("tempdir");
    for d in ["home", "proj", "stdin-dir"] {
        std::fs::create_dir_all(t.path().join(d)).expect("mkdir");
    }
    t
}

/// Run `ctxrot <sub>` with a CLEARED environment (no GUARD_DISABLE, no
/// CTXROT_*/autonomy/XDG/GIT_* leaking in): only PATH, HOME and, when given,
/// `CLAUDE_CODE_SESSION_ID`.
fn run(sub: &str, sb: &Path, stdin: Stdin<'_>, session: Option<&str>) -> Out {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_ctxrot"));
    cmd.arg(sub)
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
            cmd.output().expect("ctxrot runs")
        }
        Stdin::Bytes(b) => {
            cmd.stdin(Stdio::piped());
            let mut child = cmd.spawn().expect("ctxrot spawns");
            let _ = child.stdin.take().expect("stdin pipe").write_all(b);
            child.wait_with_output().expect("ctxrot runs")
        }
    };
    Out {
        code: out.status.code(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

fn payload(sb: &Path, extra: serde_json::Value) -> Vec<u8> {
    let mut v = serde_json::json!({
        "session_id": "stdin-read-error-control",
        "transcript_path": "",
        "cwd": sb.join("proj").to_string_lossy(),
    });
    for (k, x) in extra.as_object().expect("object").iter() {
        v[k] = x.clone();
    }
    v.to_string().into_bytes()
}

fn dir(sb: &Path) -> std::path::PathBuf {
    sb.join("stdin-dir")
}

// ---------------------------------------------------------------- A. stop

fn assert_stdin_failure_block(o: &Out, case: &str) {
    assert_eq!(
        o.code,
        Some(0),
        "{case}: a stdin read error at the ctxrot Stop gate must exit 0 with a block \
         decision (exit 101 = uncaught panic = the stop proceeds). stdout={:?} stderr={:?}",
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
                "{case}: no stdout — an empty Stop-hook stdout is an ALLOW. stderr={:?}",
                o.stderr
            )
        });
    let v: serde_json::Value = serde_json::from_str(last)
        .unwrap_or_else(|e| panic!("{case}: last stdout line is not JSON ({e}): {last:?}"));
    assert_eq!(v["decision"], "block", "{case}: got {v}");
    let reason = v["reason"].as_str().unwrap_or_default();
    assert!(
        reason.contains("stdin read failed"),
        "{case}: the block reason must name the stdin read failure, got {reason:?}"
    );
}

#[test]
fn ctxrot_stop_stdin_read_error_blocks_with_session_set() {
    let sb = sandbox();
    let o = run(
        "stop",
        sb.path(),
        Stdin::Dir(&dir(sb.path())),
        Some("stdin-read-error-sess"),
    );
    assert_stdin_failure_block(&o, "CLAUDE_CODE_SESSION_ID set (chain-first stop)");
}

#[test]
fn ctxrot_stop_stdin_read_error_blocks_with_session_unset() {
    let sb = sandbox();
    let o = run("stop", sb.path(), Stdin::Dir(&dir(sb.path())), None);
    assert_stdin_failure_block(&o, "CLAUDE_CODE_SESSION_ID unset");
}

/// Control: a readable Stop payload with no transcript → no nudge, exit 0,
/// empty stdout (allow).
#[test]
fn ctxrot_stop_valid_payload_control() {
    let sb = sandbox();
    let p = payload(
        sb.path(),
        serde_json::json!({"hook_event_name": "Stop", "stop_hook_active": false}),
    );
    let o = run(
        "stop",
        sb.path(),
        Stdin::Bytes(&p),
        Some("stdin-read-error-sess"),
    );
    assert_eq!(
        o.code,
        Some(0),
        "control: stdout={:?} stderr={:?}",
        o.stdout,
        o.stderr
    );
    assert!(o.stdout.trim().is_empty(), "control: stdout={:?}", o.stdout);
}

// ------------------------------------------------- B/D. toolguard and guard

/// Exactly one non-empty stdout line, JSON, with the given hookEventName and
/// an `additionalContext` that mentions "stdin".
fn assert_single_context_line(o: &Out, event: &str, case: &str) {
    assert_eq!(
        o.code,
        Some(0),
        "{case}: must exit 0 (stdout={:?} stderr={:?})",
        o.stdout,
        o.stderr
    );
    let lines: Vec<&str> = o.stdout.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(
        lines.len(),
        1,
        "{case}: expected exactly one JSON line on stdout announcing that the payload \
         could not be read — an empty stdout is indistinguishable from 'nothing to say'. \
         stdout={:?} stderr={:?}",
        o.stdout,
        o.stderr
    );
    let v: serde_json::Value = serde_json::from_str(lines[0])
        .unwrap_or_else(|e| panic!("{case}: stdout line is not JSON ({e}): {:?}", lines[0]));
    assert_eq!(
        v["hookSpecificOutput"]["hookEventName"], event,
        "{case}: got {v}"
    );
    let ctx = v["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap_or_default();
    assert!(
        ctx.contains("stdin"),
        "{case}: additionalContext must say the stdin payload could not be read, got {ctx:?}"
    );
}

#[test]
fn ctxrot_toolguard_stdin_read_error_announces_unknown_size() {
    let sb = sandbox();
    let o = run(
        "toolguard",
        sb.path(),
        Stdin::Dir(&dir(sb.path())),
        Some("stdin-read-error-sess"),
    );
    assert_single_context_line(&o, "PostToolUse", "toolguard read error");
}

/// Control: a small, readable PostToolUse payload → nothing to groom: exit 0,
/// empty stdout.
#[test]
fn ctxrot_toolguard_valid_payload_control() {
    let sb = sandbox();
    let p = payload(
        sb.path(),
        serde_json::json!({
            "hook_event_name": "PostToolUse",
            "tool_name": "Read",
            "tool_input": {"file_path": "/x"},
            "tool_response": {"content": "small"},
        }),
    );
    let o = run(
        "toolguard",
        sb.path(),
        Stdin::Bytes(&p),
        Some("stdin-read-error-sess"),
    );
    assert_eq!(
        o.code,
        Some(0),
        "control: stdout={:?} stderr={:?}",
        o.stdout,
        o.stderr
    );
    assert!(o.stdout.trim().is_empty(), "control: stdout={:?}", o.stdout);
}

#[test]
fn ctxrot_guard_stdin_read_error_announces_unread_payload() {
    let sb = sandbox();
    let o = run(
        "guard",
        sb.path(),
        Stdin::Dir(&dir(sb.path())),
        Some("stdin-read-error-sess"),
    );
    assert_single_context_line(&o, "UserPromptSubmit", "guard read error");
}

/// Control: a readable UserPromptSubmit payload with no transcript → nothing
/// to inject: exit 0, empty stdout.
#[test]
fn ctxrot_guard_valid_payload_control() {
    let sb = sandbox();
    let p = payload(
        sb.path(),
        serde_json::json!({"hook_event_name": "UserPromptSubmit", "prompt": "hello"}),
    );
    let o = run(
        "guard",
        sb.path(),
        Stdin::Bytes(&p),
        Some("stdin-read-error-sess"),
    );
    assert_eq!(
        o.code,
        Some(0),
        "control: stdout={:?} stderr={:?}",
        o.stdout,
        o.stderr
    );
    assert!(o.stdout.trim().is_empty(), "control: stdout={:?}", o.stdout);
}

// ------------------------------------------------------------ C. statusline

/// What the statusline prints for a non-empty payload it could not read
/// (today: unparseable stdin → `usage::unknown_line`). Asserted non-empty and
/// carrying the unknown marker, so the comparison below is not vacuous.
fn unknown_band(sb: &Path) -> String {
    let o = run(
        "statusline",
        sb,
        Stdin::Bytes(b"not json"),
        Some("stdin-read-error-sess"),
    );
    assert_eq!(
        o.code,
        Some(0),
        "control: stdout={:?} stderr={:?}",
        o.stdout,
        o.stderr
    );
    let line = o.stdout.trim_end().to_string();
    assert!(
        line.contains("?%") && line.contains("unknown"),
        "control: unparseable stdin must render the explicit unknown band, got {line:?}"
    );
    line
}

#[test]
fn ctxrot_statusline_unparseable_stdin_control() {
    let sb = sandbox();
    let _ = unknown_band(sb.path());
}

#[test]
fn ctxrot_statusline_stdin_read_error_renders_unknown_band() {
    let sb = sandbox();
    let expected = unknown_band(sb.path());
    let o = run(
        "statusline",
        sb.path(),
        Stdin::Dir(&dir(sb.path())),
        Some("stdin-read-error-sess"),
    );
    assert_eq!(
        o.code,
        Some(0),
        "statusline must not crash on a stdin read error (exit 101 renders a blank bar, \
         read as 'nothing to report'). stdout={:?} stderr={:?}",
        o.stdout,
        o.stderr
    );
    assert_eq!(
        o.stdout.trim_end(),
        expected,
        "a stdin read error must render the same explicit unknown band as an unreadable \
         payload; stderr={:?}",
        o.stderr
    );
}
