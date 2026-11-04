// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `fde evidence verify` and PASS-only requirements.
//!
//! Written by an independent test author; the implementer must not edit these tests.
//!
//! Contract under test (user ruling 2026-11-04: PASS-only is not complete):
//!
//! * `verify` exits 1 when any requirement's only evidence is PASS (a
//!   determined negative); stderr names that requirement and says it is
//!   PASS-only (contains the id and the substring `PASS`).
//! * RED then GREEN is complete; PASS -> RED -> GREEN is complete (GREEN
//!   after RED).
//! * Mixed: R1 RED+GREEN with R2 PASS-only is exit 1.
//! * An unratified target stays exit 2 for verify (unchanged).

mod common;

use std::fs;
use std::process::Output;

use common::{code, show, stderr, Engagement};

fn assert_code(out: &Output, want: i32, what: &str) {
    assert_eq!(code(out), Some(want), "{what}: {}", show(out));
}

fn ev(e: &Engagement, kind: &str, req: &str, cmd: &str) -> Output {
    e.fde(&["evidence", kind, "--req", req, "--cmd", cmd])
}

fn verify(e: &Engagement) -> Output {
    e.fde(&["evidence", "verify"])
}

/// RED then GREEN for `req` via `test -f <flag>` at the target root.
fn red_green(e: &Engagement, req: &str, flag: &str) {
    let cmd = format!("test -f {flag}");
    assert_code(&ev(e, "red", req, &cmd), 0, &format!("RED {req}"));
    fs::write(e.root().join(flag), "").unwrap();
    assert_code(&ev(e, "green", req, &cmd), 0, &format!("GREEN {req}"));
}

/// verify exit 1 whose stderr names `req` and says PASS.
fn assert_pass_only_reason(out: &Output, req: &str, what: &str) {
    assert_code(out, 1, what);
    let err = stderr(out);
    assert!(
        err.contains(req),
        "{what}: stderr must name {req}: {}",
        show(out)
    );
    assert!(
        err.contains("PASS"),
        "{what}: stderr must say the requirement is PASS-only: {}",
        show(out)
    );
}

#[test]
fn single_pass_only_requirement_verify_exits_1_naming_it() {
    let e = Engagement::ratified(&[("R1", "first")]);
    assert_code(&ev(&e, "pass", "R1", "true"), 0, "PASS R1");
    assert_pass_only_reason(&verify(&e), "R1", "PASS-only R1");
}

#[test]
fn mixed_red_green_and_pass_only_verify_exits_1_naming_the_pass_only_one() {
    let e = Engagement::ratified(&[("R1", "first"), ("R2", "second")]);
    red_green(&e, "R1", "done-r1");
    assert_code(&ev(&e, "pass", "R2", "true"), 0, "PASS R2");
    let v = verify(&e);
    assert_pass_only_reason(&v, "R2", "R1 RED+GREEN, R2 PASS-only");
}

#[test]
fn red_then_green_is_complete_control() {
    let e = Engagement::ratified(&[("R1", "first")]);
    red_green(&e, "R1", "done-r1");
    assert_code(&verify(&e), 0, "RED then GREEN is complete");
}

#[test]
fn pass_then_red_then_green_is_complete() {
    let e = Engagement::ratified(&[("R1", "first")]);
    assert_code(&ev(&e, "pass", "R1", "true"), 0, "PASS R1");
    assert_pass_only_reason(&verify(&e), "R1", "PASS alone");
    red_green(&e, "R1", "done-r1");
    assert_code(&verify(&e), 0, "PASS -> RED -> GREEN is complete");
}

#[test]
fn pass_only_does_not_mask_a_missing_requirement() {
    let e = Engagement::ratified(&[("R1", "first"), ("R2", "second")]);
    assert_code(&ev(&e, "pass", "R1", "true"), 0, "PASS R1");
    // R2 has no evidence at all: still exit 1.
    assert_code(&verify(&e), 1, "R1 PASS-only, R2 unmeasured");
}

#[test]
fn unratified_verify_stays_exit_2() {
    let e = Engagement::new(&[("R1", "first")]);
    e.begin();
    let v = verify(&e);
    assert_code(&v, 2, "unratified target");
    assert!(!stderr(&v).trim().is_empty(), "{}", show(&v));
}
