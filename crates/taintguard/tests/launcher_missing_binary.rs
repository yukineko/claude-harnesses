//! `bin/taintguard` (the POSIX launcher) — what happens when NO per-platform
//! binary is bundled for the host.
//!
//! This is a NEW launcher (taintguard previously shipped no `bin/` at all, so
//! `enabledPlugins` could not activate it — see backlog 4ee2b335). Per
//! CLAUDE.md DoD10 ("新規 gate crate は誕生時点から verdict 三値化の作法で実装
//! される"), it is written to the ALREADY-CORRECTED pattern
//! (`crates/ctxrot/bin/ctxrot`, fixed 2026-07-27) rather than the older
//! blanket-`exit 0` shape a from-scratch copy of `crates/blastguard/bin/blastguard`
//! would have reintroduced.
//!
//! * `gate` — PreToolUse, verdict-bearing: silence is indistinguishable from
//!   "session is not tainted" (an allow). The launcher must ask, not allow.
//! * `mark` / `clear` — PostToolUse / Stop, state-mutation with no
//!   `permissionDecision` channel: must not stay silent, but do not need a
//!   JSON verdict — see the launcher's own comment for why `gate`'s ask
//!   already covers the missing-mark window (one binary, one missing-binary
//!   condition) and why a missed `clear` fails closed on its own (taint
//!   marker stays SET, the restrictive direction).

// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Write;
use std::process::{Command, Stdio};

#[derive(Debug)]
struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

/// Run the real launcher from a directory that provably contains no
/// `taintguard-<os>-<arch>` sibling, so the missing-binary path is taken on
/// every platform (the repo's own `bin/` may hold a built binary once this
/// task's rollout has run).
fn run(args: &[&str], stdin: &str) -> Run {
    let dir = tempfile::tempdir().expect("tempdir");
    let launcher = dir.path().join("taintguard");
    std::fs::copy(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("bin")
            .join("taintguard"),
        &launcher,
    )
    .expect("copy launcher");

    let mut child = Command::new("sh")
        .arg(&launcher)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn sh");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(stdin.as_bytes())
        .expect("write stdin");
    let out = child.wait_with_output().expect("wait");
    Run {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

#[test]
fn gate_missing_binary_asks_instead_of_silently_allowing() {
    let r = run(&["gate"], r#"{"tool_name":"Bash"}"#);
    assert_eq!(r.code, 0, "PreToolUse decisions ride stdout, not exit code");
    assert!(
        !r.stdout.trim().is_empty(),
        "silence IS the allow — the launcher must emit a decision: {r:?}",
    );
    let v: serde_json::Value = serde_json::from_str(r.stdout.trim())
        .unwrap_or_else(|e| panic!("stdout must be hook JSON ({e}): {}", r.stdout));
    assert_eq!(
        v["hookSpecificOutput"]["hookEventName"], "PreToolUse",
        "{v}"
    );
    assert_eq!(
        v["hookSpecificOutput"]["permissionDecision"], "ask",
        "cannot-determine-tainted must resolve to ask, not allow: {v}",
    );
    let reason = v["hookSpecificOutput"]["permissionDecisionReason"]
        .as_str()
        .unwrap_or_default();
    assert!(
        reason.contains("taintguard"),
        "the reason is the only steering channel; name the cause: {reason}",
    );
}

#[test]
fn mark_missing_binary_is_not_silent_but_exits_zero() {
    let r = run(&["mark"], r#"{"tool_name":"WebFetch"}"#);
    assert_eq!(r.code, 0, "PostToolUse hooks must never break the turn");
    assert!(
        r.stderr.contains("taintguard") && r.stderr.contains("mark"),
        "a skipped mark must say so, not disappear: stderr={}",
        r.stderr,
    );
}

#[test]
fn clear_missing_binary_is_not_silent_but_exits_zero() {
    let r = run(&["clear"], r#"{"session_id":"s1"}"#);
    assert_eq!(r.code, 0, "Stop hooks must never break the turn");
    assert!(
        r.stderr.contains("taintguard") && r.stderr.contains("clear"),
        "a skipped clear must say so, not disappear: stderr={}",
        r.stderr,
    );
}

#[test]
fn unknown_subcommand_exits_nonzero_rather_than_printing_empty_data() {
    let r = run(&["not-a-real-subcommand"], "");
    assert_ne!(
        r.code, 0,
        "an unrecognised CLI call must not look like clean empty output"
    );
}
