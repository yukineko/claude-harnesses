#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Review finding on 2df12aeef (condukt run run-20261117-100335-22251, t2),
//! unverified when this file was written:
//! `judge_words` tags a `tdd green --cmd <c>` test with `TDD_GREEN_ROUTE` once
//! per nesting level, while `check_rerun` strips the tag only once to build the
//! record-only bare key. So for
//! `tdd green --cmd "tdd green --cmd '<repro>'"` the record-only key is
//! `tdd green --cmd <repro>` (still tagged) and the bare key of `<repro>` is
//! never recorded: a later bare `<repro>` on the same tree is allowed, and a
//! green with a different nesting depth gets a different checked key, so it is
//! never refused as a rerun. Before 2df12aeef every nesting level judged to the
//! bare key, so both were refused.
//!
//! Asserted (one session, one tree, Pre + confirming Post for each allowed run):
//! (a) after a nested `tdd green` of `<repro>`, a bare `<repro>` is denied;
//! (b) a `tdd green` of `<repro>` at a different nesting depth after a first
//!     `tdd green` of `<repro>` is denied (1 then 2, 2 then 3, 2 then 1).
//! Control: after a non-nested `tdd green` of `<repro>`, a bare `<repro>` is
//! denied (the case 2df12aeef handles).
//!
//! DOES NOT PROVE: whether nesting `tdd green` is reachable or meaningful in
//! practice (the `tdd` binary is never run here; only the PreToolUse judgement
//! of the command line is observed); which fix is right (normalize away every
//! tag, refuse nesting, ...); behaviour across trees or sessions; `tdd red`
//! nesting (not tagged by 2df12aeef, not exercised).
//!
//! RED written by an independent test author, not the implementer.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use tempfile::TempDir;

const REPRO: &str = "cargo test -p x --test t";
const GREEN_1: &str = "tdd green --task t --cmd \"cargo test -p x --test t\"";
const GREEN_2: &str =
    "tdd green --task t --cmd \"tdd green --task t --cmd 'cargo test -p x --test t'\"";
const GREEN_3: &str = "tdd green --task t --cmd \"tdd green --task t --cmd \\\"tdd green --task t --cmd 'cargo test -p x --test t'\\\"\"";

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

/// Control: a non-nested `tdd green` of the repro records its bare key, so a
/// bare run of the repro afterwards on the same tree is refused.
#[test]
fn control_bare_after_single_tdd_green_is_denied() {
    let e = env();
    ran(&e, GREEN_1, "tu-1");
    let second = pre(&e, REPRO, "tu-2");
    assert!(
        denied(&second),
        "bare run after tdd green, same tree, must be refused: {second:?}"
    );
}

/// (a) The nested route must record the bare key too.
#[test]
fn a_bare_after_nested_tdd_green_is_denied() {
    let e = env();
    ran(&e, GREEN_2, "tu-1");
    let second = pre(&e, REPRO, "tu-2");
    assert!(
        denied(&second),
        "bare run after a nested tdd green, same tree, was not refused (rerun guard bypassed): {second:?}"
    );
}

/// (b) depth 1 then depth 2 on the same tree: the second green is a rerun.
#[test]
fn b_nested_tdd_green_after_single_tdd_green_is_denied() {
    let e = env();
    ran(&e, GREEN_1, "tu-1");
    let second = pre(&e, GREEN_2, "tu-2");
    assert!(
        denied(&second),
        "depth-2 tdd green after depth-1 tdd green, same tree, was not refused: {second:?}"
    );
}

/// (b) depth 2 then depth 3 on the same tree: the second green is a rerun.
#[test]
fn b_deeper_tdd_green_after_nested_tdd_green_is_denied() {
    let e = env();
    ran(&e, GREEN_2, "tu-1");
    let second = pre(&e, GREEN_3, "tu-2");
    assert!(
        denied(&second),
        "depth-3 tdd green after depth-2 tdd green, same tree, was not refused: {second:?}"
    );
}

/// (b) depth 2 then depth 1 on the same tree: the second green is a rerun.
#[test]
fn b_single_tdd_green_after_nested_tdd_green_is_denied() {
    let e = env();
    ran(&e, GREEN_2, "tu-1");
    let second = pre(&e, GREEN_1, "tu-2");
    assert!(
        denied(&second),
        "depth-1 tdd green after depth-2 tdd green, same tree, was not refused: {second:?}"
    );
}
