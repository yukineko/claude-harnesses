// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! End-to-end tests for the real built `tdd` binary.
//!
//! tdd is a clap CLI. `gate` is the Stop hook — it must NEVER break a turn:
//! garbage stdin (a *harness* error) exits 0 and allows the stop. `verify` is a
//! deterministic read-only gate that exits 1 when no proofs exist.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

use serde_json::Value;

/// Run the binary with `args` and `payload` on stdin, inside an isolated cwd +
/// HOME. Returns (exit_code, stdout).
fn run(args: &[&str], payload: &str) -> (i32, String) {
    let bin = env!("CARGO_BIN_EXE_tdd");
    let dir = unique_dir();
    std::fs::create_dir_all(&dir).unwrap();
    let mut child = Command::new(bin)
        .args(args)
        .current_dir(&dir)
        .env("HOME", &dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("binary spawns");
    if let Some(mut child_stdin) = child.stdin.take() {
        let _ = child_stdin.write_all(payload.as_bytes());
    }
    let out = child.wait_with_output().expect("binary runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

fn unique_dir() -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let id = N.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("tdd-it-{}-{}", std::process::id(), id))
}

#[test]
fn help_describes_the_gate() {
    let (code, stdout) = run(&["--help"], "");
    assert_eq!(code, 0, "--help must exit 0");
    assert!(
        stdout.contains("Test-first gate for Claude Code"),
        "expected the about string, got: {stdout}"
    );
}

#[test]
fn gate_malformed_stdin_allows_the_stop() {
    // The load-bearing invariant: a harness-level error (garbage stdin) must
    // never trap a turn. `gate` treats unparseable input as interactive and the
    // empty isolated cwd has no config, so it allows the stop with exit 0.
    let (code, _stdout) = run(&["gate"], "not json");
    assert_eq!(code, 0, "gate must allow the stop on malformed stdin");
}

#[test]
fn gate_valid_hook_payload_exits_zero() {
    // A valid Stop HookInput pointed at an empty (non-git) isolated cwd: nothing
    // changed, so there is nothing to block — exit 0.
    let payload = r#"{"hook_event_name":"Stop","cwd":"."}"#;
    let (code, _stdout) = run(&["gate"], payload);
    assert_eq!(code, 0, "gate must exit 0 toward Claude");
}

#[test]
fn verify_without_proofs_exits_one() {
    // Deterministic read-only gate: with no RED/GREEN proof artifacts present,
    // `verify` reports the missing proof and exits 1.
    let (code, _stdout) = run(&["verify", "--task", "demo"], "");
    assert_eq!(code, 1, "verify exits 1 when proofs are absent");
}

// ── `tdd oracle` end-to-end: the F→P reproduction gate ─────────────────────
//
// These drive the REAL built binary (not `transition::oracle_report` directly)
// so the CLI wiring (proof file discovery, JSON printing, exit code) is
// covered, not just the pure classification logic already unit-tested in
// `src/transition.rs`.

/// Write a `<task>.<kind>.json` proof artifact (the shape `tdd red`/`tdd green`
/// produce, reduced to the one field `tdd oracle` reads: `passed`) directly
/// under `<dir>/.tdd/`, the default `proof_dir`.
fn write_proof(dir: &Path, task: &str, kind: &str, passed: bool) {
    let proof_dir = dir.join(".tdd");
    std::fs::create_dir_all(&proof_dir).expect("create proof dir");
    let path = proof_dir.join(format!("{task}.{kind}.json"));
    std::fs::write(&path, serde_json::json!({ "passed": passed }).to_string())
        .expect("write proof artifact");
}

/// Run `tdd oracle --task <task>` in a fresh isolated dir, having first
/// written the given RED/GREEN proofs (when `Some`). Returns
/// `(exit_code, parsed stdout JSON)`.
fn run_oracle(task: &str, red: Option<bool>, green: Option<bool>) -> (i32, Value) {
    let bin = env!("CARGO_BIN_EXE_tdd");
    let dir = unique_dir();
    std::fs::create_dir_all(&dir).unwrap();
    if let Some(passed) = red {
        write_proof(&dir, task, "red", passed);
    }
    if let Some(passed) = green {
        write_proof(&dir, task, "green", passed);
    }
    let child = Command::new(bin)
        .args(["oracle", "--task", task])
        .current_dir(&dir)
        .env("HOME", &dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("binary spawns");
    let out = child.wait_with_output().expect("binary runs");
    let code = out.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let json: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("oracle stdout not valid JSON: {e}\nstdout: {stdout:?}"));
    (code, json)
}

#[test]
fn oracle_reports_valid_fail_to_pass_and_exits_zero() {
    // The genuine, load-bearing case: the test failed before the fix (RED) and
    // passes after it (GREEN) — the only transition that proves test-first.
    let (code, json) = run_oracle("demo-fp", Some(false), Some(true));
    assert_eq!(code, 0, "a valid F→P oracle must exit 0, got {json}");
    assert_eq!(json["transition"], "fail_to_pass");
    assert_eq!(json["valid_fp_oracle"], true);
    assert_eq!(json["has_red"], true);
    assert_eq!(json["has_green"], true);
}

#[test]
fn oracle_rejects_a_transition_that_never_failed_first() {
    // RED already passed (not test-first) even though GREEN also passes: this
    // must be refused, not accepted as an F→P oracle.
    let (code, json) = run_oracle("demo-pass-to-pass", Some(true), Some(true));
    assert_eq!(code, 1, "a non-F→P transition must exit 1, got {json}");
    assert_eq!(json["transition"], "pass_to_pass");
    assert_eq!(json["valid_fp_oracle"], false);
}

#[test]
fn oracle_rejects_a_still_failing_green() {
    // RED failed (good) but GREEN still fails: no proof the fix worked.
    let (code, json) = run_oracle("demo-fail-to-fail", Some(false), Some(false));
    assert_eq!(code, 1, "fail_to_fail must exit 1, got {json}");
    assert_eq!(json["transition"], "fail_to_fail");
    assert_eq!(json["valid_fp_oracle"], false);
}

#[test]
fn oracle_missing_proofs_is_fail_soft_never_panics() {
    // No RED/GREEN proof artifacts at all: must degrade gracefully to
    // `"unknown"` / `valid_fp_oracle:false` rather than panicking, and still
    // print parseable JSON (fail-soft, not a harness crash).
    let (code, json) = run_oracle("demo-missing", None, None);
    assert_eq!(code, 1, "missing proofs must exit 1, got {json}");
    assert_eq!(json["transition"], "unknown");
    assert_eq!(json["valid_fp_oracle"], false);
    assert_eq!(json["has_red"], false);
    assert_eq!(json["has_green"], false);
}

#[test]
fn oracle_unreadable_proof_exits_2_undetermined() {
    // A proof path that exists but cannot be read (a directory at the GREEN
    // artifact path => EISDIR, not NotFound) is "could not look", not "looked
    // and found nothing": exit 2, `undetermined`, never a valid oracle.
    let bin = env!("CARGO_BIN_EXE_tdd");
    let dir = unique_dir();
    std::fs::create_dir_all(&dir).unwrap();
    write_proof(&dir, "demo-unreadable", "red", false);
    std::fs::create_dir_all(dir.join(".tdd").join("demo-unreadable.green.json")).unwrap();
    let out = Command::new(bin)
        .args(["oracle", "--task", "demo-unreadable"])
        .current_dir(&dir)
        .env("HOME", &dir)
        .stdin(Stdio::null())
        .output()
        .expect("binary runs");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let json: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("oracle stdout not valid JSON: {e}\nstdout: {stdout:?}"));
    assert_eq!(
        out.status.code(),
        Some(2),
        "unreadable proof must exit 2, got {json}"
    );
    assert_eq!(json["transition"], "undetermined");
    assert_eq!(json["valid_fp_oracle"], false);
    assert_eq!(json["has_red"], true);
    assert!(
        json["has_green"].is_null(),
        "has_green must be null: {json}"
    );
}
