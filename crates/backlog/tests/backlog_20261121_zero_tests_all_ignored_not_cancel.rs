#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro (2026-11-21, observed live at main 382b10028 on task 1f037eb5):
//! `backlog next --claim` re-ran the stored repro
//! `cargo test -p blastguard --test backlog_20261121_rg_pre_protected` (no
//! `-- --ignored`). Every test in that file is `#[ignore]`d, so libtest ran
//! ZERO tests and exited 0 (`0 passed; 0 failed; N ignored`). With
//! `-- --ignored` the same file reports `0 passed; 2 failed` (the defect IS
//! reproduced). `evidence::zero_tests_ran` -> `RecheckVerdict::NotDemonstrated`
//! -> `closes()` -> `claim_recheck::auto_cancel` cancelled the row, although
//! its own detail says "a run that executed no test is not a not-reproduced
//! observation".
//!
//! Shape pinned here: exit 0, `passed + failed = 0`, `ignored >= 1` — libtest
//! itself says the tests EXIST and were not run. This differs from
//! backlog_20261113_no_deferral_auto_close.rs::g_cargo_zero_tests_at_head_is_auto_cancelled
//! (`0 passed; 0 failed; 0 ignored`, nothing exists) and from
//! backlog_20261116_zero_tests_filtered_out_not_discard.rs (`N filtered out`).
//!
//! Reading (a), asserted here: a run that executed no test observed nothing
//! about the defect (CLAUDE.md §3: undetermined is not clean), so neither
//! `next --claim` nor `recheck` may close the row; it stays pending with no
//! closure (claim refused / recheck reports undeterminable).
//! Reading (b), intended per user ruling 2026-11-13 (「実証されないならclose」)
//! and the `RecheckVerdict` doc in evidence.rs ("NOT DEMONSTRATED = ... exit 0
//! with zero tests run"): the stored repro itself no longer demonstrates the
//! defect, so close. Under (b) this test is wrong. Told apart only by a human
//! ruling on whether "N ignored" (tests exist, parked) is "not demonstrated".
//!
//! Does NOT prove: anything about real libtest beyond the measured output
//! shape (the cargo here is a fake), nor what the fix should do instead
//! (refuse vs. rewrite the stored cmd with `-- --ignored`).

mod common;

/// `broken` -> the test runs and fails (as when the repro was filed);
/// `parked` -> the test was `#[ignore]`d afterwards: run without
/// `--ignored`, libtest executes nothing and exits 0.
const CARGO_PARKED: &str = r#"#!/bin/bash
m="$(cat marker 2>/dev/null)"
case "$m" in
  broken) echo "running 2 tests"; echo "test a ... FAILED"; echo "test b ... FAILED"; echo "test result: FAILED. 0 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out"; exit 101;;
  parked) echo "running 2 tests"; echo "test a ... ignored, RED(1f037eb5)"; echo "test b ... ignored, RED(1f037eb5)"; echo ""; echo "test result: ok. 0 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out; finished in 0.00s"; exit 0;;
  *)      echo "no marker"; exit 2;;
esac
"#;

/// Same shape as the live 1f037eb5 repro: `--test <file>`, no `-- --ignored`.
const CMD: &str = "cargo test -p x --test t";

/// `add` writes the minted id back into the one id-less `#[ignore = "RED..."]`
/// of the tracked tests/t.rs (and exits 1 if it cannot), so the fixture
/// carries that file, and the write-back is committed after `add` (as the
/// caller is told to) so the tree is clean for the re-run.
const RED_FILE: &str = "#[ignore = \"RED: parked\"]\n#[test]\nfn a() {}\n";

fn parked_row(f: &common::Fixture) -> String {
    f.write_exec_shim("cargo", CARGO_PARKED);
    f.write("marker", "broken\n");
    f.write("tests/t.rs", RED_FILE);
    f.commit_paths(&["marker", "tests/t.rs"], "marker broken + RED test");
    let p = f.project();
    let o = f.run(&[
        "add",
        "--title",
        "parked row",
        "--project",
        &p,
        "--repro-test",
        CMD,
    ]);
    assert_eq!(
        o.code,
        0,
        "control: add must file the RED row: {}",
        o.both()
    );
    let id = common::parse_added(&o.stdout);
    let body = std::fs::read_to_string(f.repo.join("tests/t.rs")).unwrap();
    assert!(
        body.contains(&format!("RED({id})")),
        "control: add must write the id back: {body}\n{}",
        o.both()
    );
    f.write("marker", "parked\n");
    f.commit_paths(
        &["marker", "tests/t.rs"],
        "marker parked (RED test #[ignore]d) + id write-back",
    );
    id
}

fn assert_not_closed(f: &common::Fixture, id: &str, out: &str) {
    let closure = f.row(id)["closure"].clone();
    assert!(
        closure.is_null() && f.status(id) == "pending",
        "row {id} was closed (status {}) on a run that executed ZERO tests while libtest \
         reported them ignored: {closure}\n{out}",
        f.status(id)
    );
}

// test-weakening-justified: new RED for an unfixed defect (backlog pending, 2026-11-21); the ignore is removed in the fixing commit.
#[ignore = "RED(f20d620c): next --claim auto-cancels a row whose repro ran 0 tests because all are #[ignore]d"]
#[test]
fn claim_does_not_cancel_row_whose_repro_tests_are_all_ignored() {
    let f = common::Fixture::new("zt-ignored-claim");
    let id = parked_row(&f);
    let c = f.run(&["next", "--claim"]);
    assert!(
        !c.stdout.contains(&id),
        "zero-test run handed the row out: {}",
        c.both()
    );
    assert_not_closed(&f, &id, &c.both());
}

// test-weakening-justified: new RED for an unfixed defect (backlog pending, 2026-11-21); the ignore is removed in the fixing commit.
#[ignore = "RED(f20d620c): recheck auto-cancels a row whose repro ran 0 tests because all are #[ignore]d"]
#[test]
fn recheck_does_not_cancel_row_whose_repro_tests_are_all_ignored() {
    let f = common::Fixture::new("zt-ignored-recheck");
    let id = parked_row(&f);
    let o = f.run(&["recheck"]);
    assert_not_closed(&f, &id, &o.both());
}
