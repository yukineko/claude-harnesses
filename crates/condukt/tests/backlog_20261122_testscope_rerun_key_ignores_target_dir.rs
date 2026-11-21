#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Observed 2026-11-22, flow batch #10, run run-20261121-220650-69750, task t2.
//! In a condukt task worktree at tree 66dd15dc8, `tdd green --task t2 --cmd
//! "cargo test -p blastguard --test backlog_20261121_quoted_heredoc_body_gt_fp
//! -- --include-ignored"` ran with the SHARED CARGO_TARGET_DIR. Another
//! worktree was building blastguard into the same dir, which likely overwrote
//! the binary the test executes via CARGO_BIN_EXE_blastguard, so the run failed
//! with the pre-fix verdict. The same tree with an isolated CARGO_TARGET_DIR in
//! a scratch checkout passed 4/4. Re-running in the task worktree with an
//! isolated CARGO_TARGET_DIR (user-authorised once) was refused by the
//! PreToolUse testscope hook: "rerun refused: ... already ran on this exact
//! tree ...; running the same test twice is forbidden (user ruling
//! 2026-11-07)". The rerun key (testscope/rerun.rs) is tree + command and does
//! not include CARGO_TARGET_DIR (given as a command-line env prefix).
//!
//! Two readings; this item is UNDETERMINED between them:
//! (a) DEFECT: the key should include the execution environment that decides
//!     which artifacts the test runs (build-output location), so a run whose
//!     result came from a different (contaminated) build output does not block
//!     a legitimately different run.
//! (b) INTENDED: the 2026-11-07 ruling forbids ANY rerun of the same test on
//!     the same tree, whatever the environment; the answer to a contaminated
//!     run is to change the code first (or the user lifts the ruling).
//! How to tell them apart: ask the user whether the 2026-11-07 ruling covers a
//! rerun whose only difference is CARGO_TARGET_DIR. If yes -> (b): close this
//! item as won't-do and delete this test. If no -> (a): fix the key and remove
//! the ignore.
//!
//! Asserted (one session, one tree, Pre + confirming Post for the first run):
//! after `CARGO_TARGET_DIR=/a <cmd>` ran, `CARGO_TARGET_DIR=/b <cmd>` is NOT
//! refused as a rerun. Control: `CARGO_TARGET_DIR=/a <cmd>` again IS refused.
//!
//! DOES NOT PROVE: that the shared target dir really clobbered the binary in
//! the original run (only likely); behaviour when CARGO_TARGET_DIR comes from
//! the process env rather than a command-line prefix; behaviour across trees
//! or sessions; which key shape a fix should use.
//!
//! RED written by an independent test author, not the implementer.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use tempfile::TempDir;

const CMD_A: &str = "CARGO_TARGET_DIR=/a cargo test -p x --test t";
const CMD_B: &str = "CARGO_TARGET_DIR=/b cargo test -p x --test t";

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

struct Env {
    home: TempDir,
    _ledger_dir: TempDir,
    ledger: PathBuf,
    root: TempDir,
}

fn env() -> Env {
    let home = tempfile::tempdir().unwrap();
    let ledger_dir = tempfile::tempdir().unwrap();
    let ledger = ledger_dir.path().join("ledger.tsv");
    Env {
        home,
        _ledger_dir: ledger_dir,
        ledger,
        root: repo(),
    }
}

fn send_as(e: &Env, sub: &str, kind: &str, cmd: &str, tuid: &str) -> (Option<i32>, String) {
    let cwd = e.root.path();
    let payload = serde_json::json!({
        "hook_event_name": kind,
        "session_id": "s",
        "tool_name": "Bash",
        "tool_input": {"command": cmd},
        "cwd": cwd.to_str().unwrap(),
        "tool_use_id": tuid,
    });
    let mut child = Command::new(env!("CARGO_BIN_EXE_condukt"))
        .arg(sub)
        .env("HOME", e.home.path())
        .env("CONDUKT_TESTSCOPE_LEDGER", &e.ledger)
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

fn pre(e: &Env, cmd: &str, tuid: &str) -> (Option<i32>, String) {
    send_as(e, "testscope", "PreToolUse", cmd, tuid)
}

fn denied(res: &(Option<i32>, String)) -> bool {
    res.1.contains("\"deny\"")
}

/// An allowed first run: Pre must allow it, then the PostToolUse confirms it.
fn ran(e: &Env, cmd: &str, tuid: &str) {
    let r = pre(e, cmd, tuid);
    assert!(!denied(&r), "first run of {cmd:?} must be allowed: {r:?}");
    let c = send_as(e, "testscope-confirm", "PostToolUse", cmd, tuid);
    assert_eq!(c.0, Some(0), "confirm of {cmd:?} must exit 0: {c:?}");
}

/// Control: the same command with the same CARGO_TARGET_DIR is still a rerun.
#[test]
fn control_same_target_dir_rerun_is_denied() {
    let e = env();
    ran(&e, CMD_A, "tu-1");
    let second = pre(&e, CMD_A, "tu-2");
    assert!(
        denied(&second),
        "same command, same CARGO_TARGET_DIR, same tree must be refused: {second:?}"
    );
}

/// RED: a different CARGO_TARGET_DIR is not (per reading (a)) the same run.
#[test]
#[ignore = "RED(2e03e8c3): rerun key ignores CARGO_TARGET_DIR (execution environment) — remove when fixed"]
fn different_target_dir_is_not_a_rerun() {
    let e = env();
    ran(&e, CMD_A, "tu-1");
    let second = pre(&e, CMD_B, "tu-2");
    assert!(
        !denied(&second),
        "same command with a different CARGO_TARGET_DIR was refused as a rerun: {second:?}"
    );
}
