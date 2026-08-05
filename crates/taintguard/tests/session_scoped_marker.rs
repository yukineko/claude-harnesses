// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! End-to-end proof that the taint marker is keyed by SESSION ALONE, so a mark
//! recorded under one working directory is seen by a gate running under another
//! (backlog 90d1ca1d).
//!
//! # Why this cannot be a unit test
//!
//! Through 0.1.10 the marker path was `<base>/<project_key(cwd)>/<session>/`.
//! `Bash` persists `cd` across calls, so a `cd` between the `Read` that marked
//! and the tool call that gated moved the lookup into a different bucket, found
//! nothing there, and answered `Clean` — a silent allow. The dimension that
//! disagreed was a *per-process* one, and `mark` and `gate` are genuinely
//! separate processes.
//!
//! A same-process test therefore cannot distinguish the fix from a regression
//! that keys on `std::env::current_dir()` instead of on the payload: one process
//! has one current directory, so both halves would agree no matter what. These
//! tests spawn the real binary TWICE with genuinely different process working
//! directories AND different payload `cwd` fields — the only shape in which the
//! two can disagree. The in-process half (payload `cwd` only) lives in
//! `src/main.rs`'s `a_mark_under_one_cwd_is_seen_by_a_gate_under_another_cwd`.
//!
//! Fault injection has been run against these: rewriting `state::state_dir` to
//! `state_base().join(project_key(&current_dir())).join(session)` turns
//! `a_mark_in_one_directory_is_seen_by_a_gate_in_another` RED (the gate answers
//! with an empty stdout — the silent allow) while leaving the anti-vacuity
//! control green.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

/// Run the `taintguard` binary's subcommand `sub` with the child's PROCESS
/// working directory set to `process_cwd` and a fully-cleared environment
/// (mirrors `provenance_gate.rs`'s `run`, which pins the payload `cwd` only).
///
/// `process_cwd` is the part that matters here and the part the sibling helper
/// deliberately does not control: it is what a regression keyed on
/// `current_dir()` would read.
fn run_in(
    sub: &str,
    payload: &str,
    process_cwd: &Path,
    state_dir: &Path,
    extra_env: &[(&str, &str)],
) -> (i32, String, String) {
    let bin = env!("CARGO_BIN_EXE_taintguard");
    let mut cmd = Command::new(bin);
    cmd.arg(sub);
    cmd.current_dir(process_cwd);
    cmd.env_clear();
    cmd.env("TAINTGUARD_STATE_DIR", state_dir);
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("binary spawns");
    if let Some(mut child_stdin) = child.stdin.take() {
        child_stdin
            .write_all(payload.as_bytes())
            .expect("write payload to stdin");
    }
    let out = child.wait_with_output().expect("binary runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

const HEADLESS_ENV: &[(&str, &str)] = &[("CLAUDECODE", "1"), ("CLAUDE_CODE_ENTRYPOINT", "sdk-cli")];

fn permission_decision(stdout: &str) -> Option<String> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(trimmed)
        .unwrap_or_else(|e| panic!("expected hook JSON, got {trimmed:?}: {e}"));
    v["hookSpecificOutput"]["permissionDecision"]
        .as_str()
        .map(|s| s.to_string())
}

/// Two sibling project roots (`a` and `b`, neither inside the other), one
/// external file to read, and an isolated state base.
struct Scene {
    _root: tempfile::TempDir,
    _state: tempfile::TempDir,
    a: std::path::PathBuf,
    b: std::path::PathBuf,
    external_file: std::path::PathBuf,
    state_dir: std::path::PathBuf,
}

fn scene(name: &str) -> Scene {
    let root = tempfile::Builder::new()
        .prefix(&format!("taintguard-crosscwd-{name}-"))
        .tempdir()
        .expect("tempdir");
    let state = tempfile::Builder::new()
        .prefix(&format!("taintguard-crosscwd-{name}-state-"))
        .tempdir()
        .expect("tempdir");
    let a = root.path().join("project-a");
    let b = root.path().join("project-b");
    let outside = root.path().join("outside");
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    let external_file = outside.join("untrusted.txt");
    std::fs::write(&external_file, "untrusted content").unwrap();

    // Fixture preconditions, asserted rather than assumed: the whole point is
    // that A and B are different projects, and that the read really is external
    // to A.
    assert!(!b.starts_with(&a), "B must not be inside A");
    assert!(!a.starts_with(&b), "A must not be inside B");
    assert!(
        !external_file.starts_with(&a),
        "the read must be external to A"
    );

    let state_dir = state.path().to_path_buf();
    Scene {
        _root: root,
        _state: state,
        a,
        b,
        external_file,
        state_dir,
    }
}

fn mark_read_payload(file: &Path, cwd: &Path, session: &str) -> String {
    serde_json::json!({
        "hook_event_name": "PostToolUse",
        "tool_name": "Read",
        "tool_input": {"file_path": file.to_string_lossy()},
        "cwd": cwd.to_string_lossy(),
        "session_id": session,
    })
    .to_string()
}

/// A write-class Bash command: NOT one the read-only allowlist recognises, so
/// the taint state is what decides the verdict. `git status` here would make
/// every assertion below a test of `readonly::is_readonly_bash` instead.
fn gate_write_payload(cwd: &Path, session: &str) -> String {
    serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": {"command": "touch out.txt"},
        "cwd": cwd.to_string_lossy(),
        "session_id": session,
    })
    .to_string()
}

/// THE PRIMARY ASSERTION (backlog 90d1ca1d): mark under A, gate under B, same
/// session ⇒ the gate enforces.
///
/// Both the process working directory and the payload `cwd` move from A to B
/// between the two invocations, which is exactly what a `cd` inside a turn does.
#[test]
fn a_mark_in_one_directory_is_seen_by_a_gate_in_another() {
    let s = scene("primary");
    let session = "session-cross-cwd";

    let (code, _, stderr) = run_in(
        "mark",
        &mark_read_payload(&s.external_file, &s.a, session),
        &s.a,
        &s.state_dir,
        &[],
    );
    assert_eq!(code, 0, "mark must always exit 0; stderr: {stderr}");

    let (code, stdout, _) = run_in(
        "gate",
        &gate_write_payload(&s.b, session),
        &s.b,
        &s.state_dir,
        HEADLESS_ENV,
    );
    assert_eq!(code, 0, "gate must always exit 0");
    assert_eq!(
        permission_decision(&stdout).as_deref(),
        Some("deny"),
        "a mark recorded under {} must be seen by a gate running under {} — an empty \
         stdout here is the silent allow this fix exists to remove; got {stdout:?}",
        s.a.display(),
        s.b.display(),
    );
}

/// CONTROL: the same mark, gated from the SAME directory it was made in, also
/// enforces. Without this, a failure of the primary test could be read as "the
/// mark never landed" rather than "the mark landed but was looked up in the
/// wrong place".
#[test]
fn the_same_mark_gated_from_its_own_directory_also_enforces() {
    let s = scene("same-dir");
    let session = "session-same-dir";

    let (code, _, _) = run_in(
        "mark",
        &mark_read_payload(&s.external_file, &s.a, session),
        &s.a,
        &s.state_dir,
        &[],
    );
    assert_eq!(code, 0);

    let (code, stdout, _) = run_in(
        "gate",
        &gate_write_payload(&s.a, session),
        &s.a,
        &s.state_dir,
        HEADLESS_ENV,
    );
    assert_eq!(code, 0);
    assert_eq!(
        permission_decision(&stdout).as_deref(),
        Some("deny"),
        "the mark must be visible from the directory it was made in; got {stdout:?}"
    );
}

/// ANTI-VACUITY: identical setup, DIFFERENT session id ⇒ the gate is silent.
///
/// This is what separates "the marker is keyed by session" from "the gate now
/// enforces unconditionally". Both directions are needed: the tests above would
/// pass against a gate that denied everything, and this one would pass against a
/// gate that allowed everything.
#[test]
fn a_different_session_is_not_tainted_by_that_mark() {
    let s = scene("anti-vacuity");

    let (code, _, _) = run_in(
        "mark",
        &mark_read_payload(&s.external_file, &s.a, "session-marked"),
        &s.a,
        &s.state_dir,
        &[],
    );
    assert_eq!(code, 0);

    // Gate from BOTH directories under the unmarked session: neither may
    // enforce, so the silence is attributable to the session id and not to the
    // directory.
    for cwd in [&s.a, &s.b] {
        let (code, stdout, _) = run_in(
            "gate",
            &gate_write_payload(cwd, "session-unmarked"),
            cwd,
            &s.state_dir,
            HEADLESS_ENV,
        );
        assert_eq!(code, 0);
        assert_eq!(
            permission_decision(&stdout),
            None,
            "an unmarked session must stay clean under {}; got {stdout:?}",
            cwd.display()
        );
        assert_eq!(
            stdout.trim(),
            "",
            "a clean session must print nothing at all under {}",
            cwd.display()
        );
    }
}

/// The Stop hook's `clear` reaches the marker from a THIRD directory: the whole
/// mark → gate → clear lifecycle survives a `cd` at every step. A `clear` that
/// addressed a different bucket would leave the session tainted forever, the
/// mirror-image failure of the one this release fixes.
#[test]
fn clear_from_a_third_directory_still_clears_the_marker() {
    let s = scene("clear");
    let session = "session-clear-cross-cwd";

    let (code, _, _) = run_in(
        "mark",
        &mark_read_payload(&s.external_file, &s.a, session),
        &s.a,
        &s.state_dir,
        &[],
    );
    assert_eq!(code, 0);

    // Precondition: it really is tainted before the clear, so the silence after
    // it is caused by the clear and not by the mark never landing.
    let (_, stdout, _) = run_in(
        "gate",
        &gate_write_payload(&s.b, session),
        &s.b,
        &s.state_dir,
        HEADLESS_ENV,
    );
    assert_eq!(
        permission_decision(&stdout).as_deref(),
        Some("deny"),
        "precondition: the session must be tainted before the clear"
    );

    let stop = serde_json::json!({
        "hook_event_name": "Stop",
        "cwd": s.b.to_string_lossy(),
        "session_id": session,
    })
    .to_string();
    let (code, _, stderr) = run_in("clear", &stop, &s.b, &s.state_dir, &[]);
    assert_eq!(code, 0, "clear must always exit 0; stderr: {stderr}");
    assert!(
        !stderr.contains("clear failed"),
        "clear must not report a failure: {stderr}"
    );

    // And now a gate from A — a third position relative to the mark (A) and the
    // clear (B) — is silent.
    let (code, stdout, _) = run_in(
        "gate",
        &gate_write_payload(&s.a, session),
        &s.a,
        &s.state_dir,
        HEADLESS_ENV,
    );
    assert_eq!(code, 0);
    assert_eq!(
        stdout.trim(),
        "",
        "a cleared session must gate silently regardless of directory; got {stdout:?}"
    );
}
