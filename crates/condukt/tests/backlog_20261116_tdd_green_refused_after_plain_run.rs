#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Observed 2026-11-16 (condukt run run-20261116-164820-25374, t2 worktree):
//! 1. The worker ran `cargo test -p condukt --test X -- --include-ignored` once
//!    from Bash on the fixed tree (PASS); PreToolUse testscope recorded it in the
//!    rerun ledger.
//! 2. The orchestrator ran `tdd red --task t2 --cmd "<same command>"` on the
//!    implementation-reverted tree and recorded RED (exit 101).
//! 3. `tdd green --task t2 --cmd "<same command>"` on the fixed tree (the very
//!    tree the worker ran on) was denied at PreToolUse by testscope: "rerun
//!    refused: `...` already ran on this exact tree (...); running the same test
//!    twice is forbidden (user ruling 2026-11-07). Change the code first, or
//!    move on".
//! 4. `condukt state check-oracle` (required:true) needs a GREEN evidence, and no
//!    path remains to create it; the worker's run cannot be imported as GREEN.
//!
//! Two readings: (a) defect: because the first run did not go through tdd, the
//! F->P oracle can never be completed; (b) intended: the same-tree rerun ban
//! takes priority. Tell apart: an oracle that is required:true while no path at
//! all satisfies it means the gates contradict each other.
//!
//! Asserted: on one tree, a plain `cargo test -p foo --test bar` (Pre + Post) and
//! then `tdd green --task t --cmd "cargo test -p foo --test bar"` (Pre) must not
//! be denied, i.e. a path to record GREEN evidence remains.
//! Control: sending the same plain command twice is still denied (the existing
//! rerun ban is intact).
//!
//! DOES NOT PROVE: how the fix should look (allowing tdd green, importing the
//! worker's run as GREEN, etc.); that `tdd green` itself records evidence
//! correctly; that check-oracle is the only consumer; behaviour of the ledger
//! across different trees or sessions (only one session and one tree are driven).
//!
//! RED written by an independent test author, not the implementer.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use tempfile::TempDir;

fn git(dir: &Path, args: &[&str]) {
    let st = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .current_dir(dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(st.success(), "git {args:?} failed");
}

fn repo() -> TempDir {
    let d = tempfile::tempdir().unwrap();
    git(d.path(), &["init", "-q"]);
    std::fs::write(d.path().join("a.txt"), "one\n").unwrap();
    git(d.path(), &["add", "a.txt"]);
    git(d.path(), &["commit", "-q", "-m", "init"]);
    d
}

fn send(
    sub: &str,
    home: &Path,
    ledger: &Path,
    payload: serde_json::Value,
    cwd: &Path,
) -> (Option<i32>, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_condukt"))
        .arg(sub)
        .env("HOME", home)
        .env("CONDUKT_TESTSCOPE_LEDGER", ledger)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.to_string().as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

fn event(
    kind: &str,
    home: &Path,
    ledger: &Path,
    cmd: &str,
    cwd: &Path,
    tuid: &str,
) -> (Option<i32>, String) {
    send_as("testscope", kind, home, ledger, cmd, cwd, tuid)
}

/// The PostToolUse that confirms `cmd` ran (spec testscope-confirmed-run R2:
/// input only), sent to the confirm hook.
fn confirm_event(
    home: &Path,
    ledger: &Path,
    cmd: &str,
    cwd: &Path,
    tuid: &str,
) -> (Option<i32>, String) {
    send_as(
        "testscope-confirm",
        "PostToolUse",
        home,
        ledger,
        cmd,
        cwd,
        tuid,
    )
}

fn send_as(
    sub: &str,
    kind: &str,
    home: &Path,
    ledger: &Path,
    cmd: &str,
    cwd: &Path,
    tuid: &str,
) -> (Option<i32>, String) {
    send(
        sub,
        home,
        ledger,
        serde_json::json!({
            "hook_event_name": kind,
            "session_id": "s",
            "tool_name": "Bash",
            "tool_input": {"command": cmd},
            "cwd": cwd.to_str().unwrap(),
            "tool_use_id": tuid,
        }),
        cwd,
    )
}

fn denied(res: &(Option<i32>, String)) -> bool {
    res.1.contains("\"deny\"")
}

#[test]
fn tdd_green_is_allowed_after_plain_run_on_same_tree() {
    let home = tempfile::tempdir().unwrap();
    let ledger_dir = tempfile::tempdir().unwrap();
    let ledger = ledger_dir.path().join("ledger.tsv");
    let root = repo();
    let cwd = root.path();
    let plain = "cargo test -p foo --test bar";
    let first = event("PreToolUse", home.path(), &ledger, plain, cwd, "tu-1");
    assert!(
        !denied(&first),
        "plain first Pre must be allowed: {first:?}"
    );
    confirm_event(home.path(), &ledger, plain, cwd, "tu-1");

    let green = "tdd green --task t --cmd \"cargo test -p foo --test bar\"";
    let second = event("PreToolUse", home.path(), &ledger, green, cwd, "tu-2");
    assert!(
        !denied(&second),
        "tdd green refused as rerun after a plain run; no path left to record GREEN evidence: {second:?}"
    );
}

/// Control: the identical plain command sent twice on one tree is still denied.
#[test]
fn control_identical_plain_rerun_is_still_denied() {
    let home = tempfile::tempdir().unwrap();
    let ledger_dir = tempfile::tempdir().unwrap();
    let ledger = ledger_dir.path().join("ledger.tsv");
    let root = repo();
    let cwd = root.path();
    let plain = "cargo test -p foo --test bar";
    let first = event("PreToolUse", home.path(), &ledger, plain, cwd, "tu-1");
    assert!(!denied(&first), "first Pre must be allowed: {first:?}");
    confirm_event(home.path(), &ledger, plain, cwd, "tu-1");
    let second = event("PreToolUse", home.path(), &ledger, plain, cwd, "tu-2");
    assert!(denied(&second), "plain rerun must be refused: {second:?}");
}
