#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Observed 2026-11-15 (condukt run run-20261114-161340-68728): a test command
//! that blastguard blocked at PreToolUse was later refused by testscope as
//! "already ran on this exact tree". The blocked attempt never ran.
//!
//! Mechanism (crates/condukt/src/testscope.rs `check_rerun`): the ledger line is
//! appended at PreToolUse when testscope returns Allow, before any other hook
//! (blastguard) can still block, and there is no PostToolUse confirmation.
//!
//! Two readings: (1) real defect: the ledger records an attempt that never
//! executed (no PostToolUse), so a legitimate first real run is refused;
//! (2) intended: an allowed PreToolUse counts as the run. Tell apart: the
//! refusal text claims the command "already ran", which is false when the tool
//! never ran.
//!
//! Asserted: PreToolUse for a test command, no PostToolUse (tool never ran),
//! then a second identical PreToolUse on the unchanged tree is allowed.
//! Control: the identical-rerun-refused control lives in
//! backlog_20261114_testscope_cd_tree_identity.rs (not duplicated here).
//!
//! DOES NOT PROVE: how a PostToolUse confirmation should reach testscope, or
//! that blastguard was the blocker (only the no-Post sequence is driven).
//!
//! RED written by an independent test author, not the implementer.

use std::io::Write;
use std::path::{Path, PathBuf};
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

fn run(home: &Path, ledger: &Path, cmd: &str, cwd: &Path) -> (Option<i32>, String) {
    let payload = serde_json::json!({
        "session_id": "s",
        "tool_name": "Bash",
        "tool_input": {"command": cmd},
        "cwd": cwd.to_str().unwrap(),
    });
    let mut child = Command::new(env!("CARGO_BIN_EXE_condukt"))
        .arg("testscope")
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

fn denied(res: &(Option<i32>, String)) -> bool {
    res.1.contains("\"deny\"")
}

// test-weakening-justified: new RED for an unfixed defect (2026-11-15).
#[test]
#[ignore = "RED: 2026-11-15 testscope records at PreToolUse so a never-executed attempt blocks the real run — remove when fixed"]
fn second_pretooluse_after_never_executed_first_is_allowed() {
    let home = tempfile::tempdir().unwrap();
    let ledger_dir = tempfile::tempdir().unwrap();
    let ledger = ledger_dir.path().join("ledger.tsv");
    let root = repo();
    let cmd = "cargo test -p foo --test bar";
    let first = run(home.path(), &ledger, cmd, root.path());
    assert!(
        !denied(&first),
        "first PreToolUse must be allowed: {first:?}"
    );
    // No PostToolUse is sent: the tool never ran (blocked by another hook).
    let second = run(home.path(), &ledger, cmd, root.path());
    assert!(
        !denied(&second),
        "unexecuted attempt was counted as a run: {second:?}"
    );
}
