#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro (2026-11-16, observed live on task 214b39b5): `backlog next
//! --claim` re-runs a stored `cargo test ... -- --ignored <name>` repro. After
//! the fixing commit removed `#[ignore]`, libtest filters the (now
//! non-ignored, passing) test OUT and prints
//! `test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 4 filtered out`
//! (verbatim shape measured at main bbe7b7bac). `evidence::zero_tests_ran`
//! classifies that as NotDemonstrated and `claim_recheck::auto_cancel` closes
//! the row with closure reason `discard`, although its own detail text says
//! "a run that executed no test is not a not-reproduced observation".
//!
//! Reading (a), asserted here: a run that executed NO test observed nothing
//! about the defect, so it must not be recorded as a `discard` (= "the
//! defect was not real"); the claim is refused / the row left pending, or it
//! is closed with a non-discard reason.
//! Reading (b), intended per user ruling 2026-11-13 (「実証されないならclose」)
//! and pinned for the no-`filtered out` shape by
//! backlog_20261113_no_deferral_auto_close.rs::g_cargo_zero_tests_at_head_is_auto_cancelled:
//! not demonstrated -> discard closure. Under (b) this test is wrong.
//! The two are told apart only by a human ruling on whether "filtered out"
//! (the named test EXISTS but was not selected) differs from "no such test".
//!
//! Does NOT prove: that the fix of the row landed (the fake cargo cannot
//! observe that), nor anything about `(backlog <id>)` subject matching.

mod common;

const CARGO_FILTERED: &str = r#"#!/bin/bash
m="$(cat marker 2>/dev/null)"
case "$m" in
  broken)     echo "running 1 test"; echo "test t ... FAILED"; echo "test result: FAILED. 0 passed; 1 failed; 0 ignored"; exit 101;;
  unignored)  echo "running 0 tests"; echo ""; echo "test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 4 filtered out; finished in 0.00s"; exit 0;;
  *)          echo "no marker"; exit 2;;
esac
"#;

#[test]
fn claim_does_not_discard_row_whose_ignored_repro_is_now_filtered_out() {
    let f = common::Fixture::new("zt-filtered");
    f.write_exec_shim("cargo", CARGO_FILTERED);
    f.write(
        "tests/t.rs",
        "#[test]\n#[ignore = \"RED: filtered row\"]\nfn t() {}\n",
    );
    f.commit_paths(&["tests/t.rs"], "t test");
    f.write("marker", "broken\n");
    f.commit_paths(&["marker"], "marker broken");
    let p = f.project();
    let cmd = "cargo test -p x --test t -- --ignored t";
    let o = f.run(&[
        "add",
        "--title",
        "filtered row",
        "--project",
        &p,
        "--repro-test",
        cmd,
    ]);
    assert_eq!(
        o.code,
        0,
        "control: add must file the RED row: {}",
        o.both()
    );
    let id = common::parse_added(&o.stdout);
    // `add` wrote the id back into tests/t.rs (working tree only); commit it
    // so the tree is clean for the re-run.
    f.commit_paths(&["tests/t.rs"], "t id written back");
    f.write("marker", "unignored\n");
    f.commit_paths(&["marker"], "marker unignored (fix removed #[ignore])");
    let c = f.run(&["next", "--claim"]);
    assert!(
        !c.stdout.contains(&id),
        "zero-test run handed the row out: {}",
        c.both()
    );
    let closure = f.row(&id)["closure"].clone();
    assert_ne!(
        closure["reason"], "discard",
        "row {id} was closed as discard on a run that executed ZERO tests (status {}): {closure}\n{}",
        f.status(&id),
        c.both()
    );
}
