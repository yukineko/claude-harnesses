// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! A RED/GREEN/PASS CMD that moves the target HEAD is refused at record time.
//!
//! Written by an independent test author; the implementer must not edit these tests.
//!
//! Contract under test (user ruling 2026-11-04): `fde evidence red|green|pass`
//! observes HEAD right before CMD and re-checks it after CMD. If HEAD changed
//! during CMD in any way (moved back, moved forward, not comparable) the
//! record is refused: exit 2, a stderr reason mentioning HEAD, and NOTHING
//! recorded (state dir bytes unchanged, verify state unchanged).
//!
//! Fixture: ratify at commit A, then commit B on top, so the pre-CMD HEAD (B)
//! is a descendant of A and the "R <= HEAD" check passes before and after.

mod common;

use std::fs;
use std::process::Output;

use common::{code, git, show, snapshot_files, stderr, Engagement};

fn assert_code(out: &Output, want: i32, what: &str) {
    assert_eq!(code(out), Some(want), "{what}: {}", show(out));
}

fn ev(e: &Engagement, kind: &str, cmd: &str) -> Output {
    e.fde(&["evidence", kind, "--req", "R1", "--cmd", cmd])
}

fn head(e: &Engagement) -> String {
    git(e.root(), &["rev-parse", "HEAD"]).trim().to_owned()
}

/// Ratified at A; then commit B. Returns (engagement, A, B).
fn at_b() -> (Engagement, String, String) {
    let e = Engagement::ratified(&[("R1", "first")]);
    let a = head(&e);
    fs::write(e.root().join("b.txt"), "b\n").unwrap();
    git(e.root(), &["add", "b.txt"]);
    git(e.root(), &["commit", "-q", "-m", "B"]);
    let b = head(&e);
    assert_ne!(a, b, "fixture: B differs from A");
    (e, a, b)
}

const COMMIT: &str = "git -c user.name=t -c user.email=t@t commit -q --allow-empty -m x";

/// Run `f`, require exit 2 with a HEAD reason and an unchanged state dir.
fn assert_refused_records_nothing(e: &Engagement, what: &str, f: impl FnOnce() -> Output) {
    let before = snapshot_files(&e.state.path);
    let out = f();
    assert_code(&out, 2, what);
    assert!(
        stderr(&out).contains("HEAD"),
        "{what}: stderr must mention HEAD: {}",
        show(&out)
    );
    assert_eq!(
        before,
        snapshot_files(&e.state.path),
        "{what}: nothing may be recorded (state dir changed)"
    );
}

#[test]
fn red_cmd_that_resets_head_back_is_refused() {
    let (e, a, _b) = at_b();
    let cmd = format!("git reset -q --hard {a}; exit 1");
    assert_refused_records_nothing(&e, "RED moving HEAD back", || ev(&e, "red", &cmd));
    assert_eq!(head(&e), a, "fixture: the CMD really moved HEAD back");
    assert_code(&e.fde(&["evidence", "verify"]), 1, "R1 still unmeasured");
}

#[test]
fn red_cmd_that_commits_forward_is_refused() {
    let (e, _a, b) = at_b();
    let cmd = format!("{COMMIT}; exit 1");
    assert_refused_records_nothing(&e, "RED moving HEAD forward", || ev(&e, "red", &cmd));
    assert_ne!(head(&e), b, "fixture: the CMD really moved HEAD forward");
    assert_code(&e.fde(&["evidence", "verify"]), 1, "R1 still unmeasured");
}

#[test]
fn green_cmd_that_resets_head_back_is_refused() {
    let (e, a, _b) = at_b();
    let cmd = format!("test -f done.txt && git reset -q --hard {a}");
    // RED run: test fails, the reset never runs, HEAD stays at B.
    assert_code(&ev(&e, "red", &cmd), 0, "RED (non-moving run)");
    fs::write(e.root().join("done.txt"), "").unwrap();
    assert_refused_records_nothing(&e, "GREEN moving HEAD back", || ev(&e, "green", &cmd));
    // The GREEN was not recorded: the ledger still holds exactly the RED.
    let (_root, key) = common::parse_resolved(&e.fde(&["resolve"]));
    let ledger = e.state.path.join("targets").join(key).join("evidence.json");
    let v: serde_json::Value = serde_json::from_slice(&fs::read(&ledger).unwrap()).unwrap();
    let kinds: Vec<&str> = v["records"]
        .as_array()
        .unwrap_or_else(|| panic!("evidence.json lacks array \"records\": {v}"))
        .iter()
        .map(|r| r["kind"].as_str().unwrap_or("?"))
        .collect();
    assert_eq!(kinds, vec!["red"], "only the RED may be in the ledger: {v}");
    // The CMD moved HEAD (to A) below the counted RED record (head B), so
    // R <= H <= C fails and verify is undetermined (2), not "RED only" (1).
    assert_code(
        &e.fde(&["evidence", "verify"]),
        2,
        "HEAD (A) is now below the RED record's head (B): R <= H <= C fails, verify is undetermined",
    );
}

#[test]
fn pass_cmd_that_commits_forward_is_refused() {
    let (e, _a, _b) = at_b();
    assert_refused_records_nothing(&e, "PASS moving HEAD forward", || ev(&e, "pass", COMMIT));
    assert_code(&e.fde(&["evidence", "verify"]), 1, "R1 still unmeasured");
}

#[test]
fn red_cmd_that_makes_head_unobservable_is_refused() {
    let (e, _a, _b) = at_b();
    // Pre-CMD observation is fine; the CMD corrupts .git/HEAD so the
    // post-CMD observation fails.
    let cmd = "echo garbage > .git/HEAD; exit 1";
    assert_refused_records_nothing(&e, "RED leaving HEAD unobservable", || ev(&e, "red", cmd));
}

// ------------------------------------------------------------- controls

#[test]
fn non_moving_cmds_still_record_control() {
    let (e, _a, _b) = at_b();
    assert_code(&ev(&e, "red", "test -f done.txt"), 0, "RED non-moving");
    fs::write(e.root().join("done.txt"), "").unwrap();
    assert_code(&ev(&e, "green", "test -f done.txt"), 0, "GREEN non-moving");
    assert_code(&e.fde(&["evidence", "verify"]), 0, "complete");

    let (p, _a, _b) = at_b();
    assert_code(&ev(&p, "pass", "true"), 0, "PASS non-moving");
}
