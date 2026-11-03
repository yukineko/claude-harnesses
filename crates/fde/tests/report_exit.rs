// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Exit-code contract of `fde report [TARGET]`.
//!
//! Written by an independent test author (CLAUDE.md §2(a)); the implementer
//! must match the contract, not edit these tests to pass.
//!
//! Contract (user ruling: "Exit 0 only when it is ratified and every
//! requirement is verified RED→GREEN"):
//!
//! * The report text is printed on stdout in every case it is printed today,
//!   including an unratified target.
//! * Exit 0 ONLY when the target is ratified AND every requirement of the
//!   ratified spec is "RED then GREEN" (a GREEN after a RED of the same cmd,
//!   with no open RED).
//! * Exit 1 (determined: not done), with a non-empty reason on stderr, when
//!   the target is determined unratified (not registered, registered but not
//!   ratified, spec changed since ratification), or any requirement is
//!   unresolved (RED without GREEN), unmeasured (no evidence), or PASS-only.
//! * Exit 2 for undetermined (corrupt state, zero requirements, ...).
//!
//! `--cmd` values are free of shell operators (`true`, `false`, `test -f X`)
//! so the tests do not depend on how the implementation runs them. Toggle
//! files live outside the target so the target tree stays untouched.

mod common;

use std::fs;
use std::path::PathBuf;
use std::process::Output;

use common::{changed_files, code, show, snapshot_files, stderr, stdout, Engagement, TempPlace};

fn assert_code(out: &Output, want: i32, what: &str) {
    assert_eq!(code(out), Some(want), "{what}: {}", show(out));
}

/// Exit 1 with a non-empty stderr reason.
fn assert_not_done(out: &Output, what: &str) {
    assert_code(out, 1, what);
    assert!(
        !stderr(out).trim().is_empty(),
        "{what}: exit 1 must state a reason on stderr: {}",
        show(out)
    );
}

fn ev(e: &Engagement, kind: &str, req: &str, cmd: &str) -> Output {
    e.fde(&["evidence", kind, "--req", req, "--cmd", cmd])
}

/// A `test -f <flag>` command whose flag file lives outside the target.
fn flag_cmd(e: &Engagement, name: &str) -> (PathBuf, String) {
    let flag = e.spec_dir.path.join(name);
    let cmd = format!("test -f {}", flag.to_str().unwrap());
    (flag, cmd)
}

/// Record RED (flag absent) then GREEN (flag created) for `req`.
fn red_then_green(e: &Engagement, req: &str) {
    let (flag, cmd) = flag_cmd(e, &format!("flag-{req}"));
    assert_code(&ev(e, "red", req, &cmd), 0, &format!("RED {req}"));
    fs::write(&flag, "").unwrap();
    assert_code(&ev(e, "green", req, &cmd), 0, &format!("GREEN {req}"));
}

fn report(e: &Engagement) -> Output {
    e.fde(&["report"])
}

// ---------------------------------------------------------------------------
// exit 0 (positive control for every exit-1 case below)
// ---------------------------------------------------------------------------

#[test]
fn report_all_red_then_green_exits_0() {
    let e = Engagement::ratified(&[("R1", "first"), ("R2", "second")]);
    red_then_green(&e, "R1");
    red_then_green(&e, "R2");
    assert_code(&e.fde(&["evidence", "verify"]), 0, "control: verify agrees");
    let r = report(&e);
    assert_code(&r, 0, "ratified and every requirement RED then GREEN");
    let text = stdout(&r);
    assert!(
        text.contains("R1") && text.contains("R2"),
        "report text must list the requirements:\n{text}"
    );
}

#[test]
fn report_naming_target_from_outside_all_red_then_green_exits_0() {
    let e = Engagement::ratified(&[("R1", "first")]);
    red_then_green(&e, "R1");
    let elsewhere = TempPlace::new("fde-elsewhere-");
    let r = e.fde_in(&elsewhere.path, &["report", e.root().to_str().unwrap()]);
    assert_code(&r, 0, "report TARGET, complete");
}

// ---------------------------------------------------------------------------
// exit 1: unratified (report text still printed)
// ---------------------------------------------------------------------------

#[test]
fn report_unregistered_target_exits_1() {
    let e = Engagement::new(&[("R1", "first")]);
    let r = report(&e);
    assert_not_done(&r, "not registered");
    assert!(
        stdout(&r).contains("UNRATIFIED"),
        "report text must still be printed for an unregistered target: {}",
        show(&r)
    );
}

#[test]
fn report_registered_not_ratified_exits_1_and_still_prints_unratified() {
    let e = Engagement::new(&[("R1", "first")]);
    e.begin();
    let r = report(&e);
    assert_not_done(&r, "registered but not ratified");
    assert!(
        stdout(&r).contains("UNRATIFIED"),
        "report text must still say UNRATIFIED: {}",
        show(&r)
    );
}

#[test]
fn report_unratified_target_named_from_outside_exits_1() {
    let e = Engagement::new(&[("R1", "first")]);
    e.begin();
    let elsewhere = TempPlace::new("fde-elsewhere-");
    let r = e.fde_in(&elsewhere.path, &["report", e.root().to_str().unwrap()]);
    assert_not_done(&r, "report TARGET, registered but not ratified");
    assert!(stdout(&r).contains("UNRATIFIED"), "{}", show(&r));
}

#[test]
fn report_spec_edited_after_ratification_exits_1_even_with_complete_evidence() {
    let e = Engagement::ratified(&[("R1", "first")]);
    red_then_green(&e, "R1");
    // Control: complete before the edit.
    assert_code(&report(&e), 0, "control: complete before the spec edit");

    let mut text = fs::read_to_string(&e.spec).unwrap();
    text.push_str("\n# edited after ratification\n");
    fs::write(&e.spec, &text).unwrap();
    assert_code(&e.fde(&["status"]), 1, "control: status says unratified");

    let r = report(&e);
    assert_not_done(&r, "spec changed since ratification");
    assert!(stdout(&r).contains("UNRATIFIED"), "{}", show(&r));
}

// ---------------------------------------------------------------------------
// exit 1: ratified but evidence incomplete
// ---------------------------------------------------------------------------

#[test]
fn report_ratified_with_no_evidence_exits_1() {
    let e = Engagement::ratified(&[("R1", "first")]);
    let r = report(&e);
    assert_not_done(&r, "ratified, zero evidence");
    assert!(
        stdout(&r).contains("R1"),
        "report text must still list R1: {}",
        show(&r)
    );
}

#[test]
fn report_red_only_exits_1() {
    let e = Engagement::ratified(&[("R1", "first")]);
    let (_flag, cmd) = flag_cmd(&e, "never-created");
    assert_code(&ev(&e, "red", "R1", &cmd), 0, "RED R1");
    let r = report(&e);
    assert_not_done(&r, "RED without GREEN");
    assert!(stdout(&r).contains("R1"), "{}", show(&r));
}

#[test]
fn report_pass_only_exits_1() {
    let e = Engagement::ratified(&[("R1", "first")]);
    assert_code(&ev(&e, "pass", "R1", "true"), 0, "PASS R1");
    // user ruling 2026-11-04: PASS-only is not complete (verify agrees with report).
    assert_code(&e.fde(&["evidence", "verify"]), 1, "control: verify");
    let r = report(&e);
    assert_not_done(&r, "PASS-only is not verified RED then GREEN");
    assert!(stdout(&r).contains("R1"), "{}", show(&r));
}

#[test]
fn report_one_red_then_green_one_unmeasured_exits_1() {
    let e = Engagement::ratified(&[("R1", "first"), ("R2", "second")]);
    red_then_green(&e, "R1");
    let r = report(&e);
    assert_not_done(&r, "R2 unmeasured");
    // Control: completing R2 flips it to 0, so the 1 is about R2.
    red_then_green(&e, "R2");
    assert_code(&report(&e), 0, "control: R2 completed");
}

#[test]
fn report_one_red_then_green_one_pass_exits_1() {
    let e = Engagement::ratified(&[("R1", "first"), ("R2", "second")]);
    red_then_green(&e, "R1");
    assert_code(&ev(&e, "pass", "R2", "true"), 0, "PASS R2");
    assert_not_done(&report(&e), "R2 is PASS-only");
}

#[test]
fn report_open_red_after_green_exits_1() {
    let e = Engagement::ratified(&[("R1", "first")]);
    red_then_green(&e, "R1");
    assert_code(&report(&e), 0, "control: R1 RED then GREEN");
    let (_flag, cmd) = flag_cmd(&e, "second-never-created");
    assert_code(&ev(&e, "red", "R1", &cmd), 0, "second RED R1");
    assert_not_done(&report(&e), "R1 has an open RED");
}

// ---------------------------------------------------------------------------
// exit 2: undetermined
// ---------------------------------------------------------------------------

#[test]
fn report_corrupt_ratification_state_exits_2() {
    let e = Engagement::new(&[("R1", "first")]);
    e.begin();
    let before = snapshot_files(&e.state.path);
    e.ratify();
    red_then_green(&e, "R1");
    assert_code(&report(&e), 0, "control: intact state, complete");
    let after = snapshot_files(&e.state.path);
    let written = changed_files(&e.state.path, &before, &after);
    assert!(!written.is_empty(), "ratify/evidence wrote no state file");
    for p in &written {
        fs::write(p, b"{{{ \0 not json ]]] = = not toml \0\n").unwrap();
    }
    let r = report(&e);
    assert_code(&r, 2, "corrupt state is undetermined");
    assert!(!stderr(&r).trim().is_empty(), "{}", show(&r));
}

#[test]
fn report_zero_requirement_spec_exits_2() {
    let e = Engagement::ratified(&[]);
    let r = report(&e);
    assert_code(&r, 2, "ratified zero-requirement spec is undetermined");
    assert!(!stderr(&r).trim().is_empty(), "{}", show(&r));
}
