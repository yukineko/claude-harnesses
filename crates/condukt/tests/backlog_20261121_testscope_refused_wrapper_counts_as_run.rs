#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Observed 2026-11-21 (main 484608e14): `tdd red ... --cmd "cargo test ..."`
//! was refused by tdd itself ("run `tdd trust`", exit 1) before cargo ran; the
//! next attempt was refused by testscope "already ran on this exact tree".
//! Mechanism: testscope-confirm promotes the pending row on PostToolUseFailure
//! too (rerun.rs `confirm_post`, any exit status) and never inspects the error.
//! Readings: (a) defect: a refused wrapper is not a run; (b) intended: Post
//! cannot see whether the inner command ran. Asserted: Pre, PostToolUseFailure
//! carrying the wrapper refusal, then identical Pre is not denied.
//! DOES NOT PROVE: how the refusal should be detected.

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

fn pre(home: &Path, ledger: &Path, cmd: &str, cwd: &Path, tuid: &str) -> (Option<i32>, String) {
    send(
        "testscope",
        home,
        ledger,
        serde_json::json!({
            "hook_event_name": "PreToolUse",
            "session_id": "s",
            "tool_name": "Bash",
            "tool_input": {"command": cmd},
            "cwd": cwd.to_str().unwrap(),
            "tool_use_id": tuid,
        }),
        cwd,
    )
}

fn post_failure(
    home: &Path,
    ledger: &Path,
    cmd: &str,
    cwd: &Path,
    err: &str,
) -> (Option<i32>, String) {
    send(
        "testscope-confirm",
        home,
        ledger,
        serde_json::json!({
            "hook_event_name": "PostToolUseFailure",
            "session_id": "s",
            "tool_name": "Bash",
            "tool_input": {"command": cmd},
            "cwd": cwd.to_str().unwrap(),
            "tool_use_id": "tu-1",
            "error": err,
        }),
        cwd,
    )
}

fn denied(res: &(Option<i32>, String)) -> bool {
    res.1.contains("\"deny\"")
}

// test-weakening-justified: new RED for an unfixed defect (2026-11-21).
#[ignore = "RED: testscope confirms a PostToolUseFailure of a wrapper that refused before the test ran"]
#[test]
fn wrapper_refused_before_test_ran_is_not_a_confirmed_run() {
    let home = tempfile::tempdir().unwrap();
    let ledger_dir = tempfile::tempdir().unwrap();
    let ledger = ledger_dir.path().join("ledger.tsv");
    let root = repo();
    let cmd = "tdd red --task t1 --author X --cmd \"cargo test -p foo --test bar baz -- --include-ignored\"";
    let first = pre(home.path(), &ledger, cmd, root.path(), "tu-1");
    assert!(!denied(&first), "first Pre must be allowed: {first:?}");
    post_failure(
        home.path(),
        &ledger,
        cmd,
        root.path(),
        "Exit code 1\nRemedy: run `tdd trust` in this root ...",
    );
    let second = pre(home.path(), &ledger, cmd, root.path(), "tu-2");
    assert!(
        !denied(&second),
        "test never executed (wrapper refused) yet rerun refused as already ran: {second:?}"
    );
}
