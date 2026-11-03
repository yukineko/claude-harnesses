//! `backlog edit <id> --repro-test "<cmd>"` replaces a pending task's stored
//! repro command (feature: intentional RED repros move under
//! `#[ignore = "backlog <id>: ..."]`, so their stored repro must change to
//! `cargo test ... -- --ignored`; hand-editing `.backlog/tasks.toml` would
//! bypass backlog's evidence check).
//!
//! Contract: the new command is run exactly as `backlog add --repro-test` runs
//! it; ONLY `reproduced` replaces the stored command; `not-reproduced` /
//! `undetermined` (incl. zero tests ran) and non-pending tasks are refused
//! (non-zero exit, store unchanged); a combined edit is atomic.
//!
//! Independent test writer (CLAUDE.md §2(a)). The fake `cargo` emulates
//! libtest for one `#[ignore]`d test `repro_ignored_one` whose pass/fail is
//! decided by the `marker` file at the checked-out HEAD (`broken` -> fails).
mod common;
use common::*;

const FAKE_CARGO_IGNORED: &str = r#"#!/bin/bash
m="$(cat marker 2>/dev/null)"
ignored=0
after_dd=0
skip=0
for a in "$@"; do
  if [ "$skip" = 1 ]; then skip=0; continue; fi
  if [ "$after_dd" = 0 ]; then
    case "$a" in
      --) after_dd=1;;
      -p|--package|--test|--bin|--lib) skip=1;;
    esac
  else
    case "$a" in
      --ignored|--include-ignored) ignored=1;;
    esac
  fi
done
if [ "$ignored" = 0 ]; then
  echo "running 1 test"
  echo "test repro_ignored_one ... ignored, backlog deadbeef: RED"
  echo "test result: ok. 0 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out"
  exit 0
fi
echo "running 1 test"
if [ "$m" = broken ]; then
  echo "test repro_ignored_one ... FAILED"
  echo "test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out"
  exit 101
fi
echo "test repro_ignored_one ... ok"
echo "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out"
exit 0
"#;

const OLD_CMD: &str = "bash tests/repro_yes.sh";
const NEW_IGN: &str = "cargo test -p x -- --ignored";
/// Same test run WITHOUT `--ignored`: "0 passed; 1 ignored" = zero tests ran.
const NEW_ZERO: &str = "cargo test -p x";

/// Fixture + a pending task filed with the shared reproducing script, then
/// HEAD moved so the fake ignored test has marker `marker_at_head`.
fn fixture(tag: &str, marker_at_head: &str) -> (Fixture, String) {
    let f = Fixture::new(tag);
    let id = f.add("edit me");
    f.write_exec_shim("cargo", FAKE_CARGO_IGNORED);
    f.write("marker", marker_at_head);
    f.commit_paths(&["marker"], "marker");
    (f, id)
}

fn stored_cmd(f: &Fixture, id: &str) -> String {
    f.row(id)["repro"]["cmd"]
        .as_str()
        .unwrap_or_else(|| panic!("row has no repro.cmd: {}", f.row(id)))
        .to_string()
}

#[test]
fn failing_ignored_command_replaces_stored_repro() {
    let (f, id) = fixture("er-ok", "broken");
    assert_eq!(stored_cmd(&f, &id), OLD_CMD);
    let o = f.run(&["edit", &id, "--repro-test", NEW_IGN]);
    assert_eq!(
        o.code,
        0,
        "edit --repro-test with a command that reproduces (fails behaviourally under --ignored) must succeed: {}",
        o.both()
    );
    assert_eq!(
        stored_cmd(&f, &id),
        NEW_IGN,
        "a reproduced edit must replace the stored repro command"
    );
    assert_eq!(f.status(&id), "pending");
}

#[test]
fn passing_command_is_refused_as_not_reproduced_and_store_unchanged() {
    let (f, id) = fixture("er-pass", "fixed");
    let before = f.store_bytes();
    let o = f.run(&["edit", &id, "--repro-test", NEW_IGN]);
    assert_ne!(
        o.code,
        0,
        "a repro command that passes (not-reproduced) must be refused: {}",
        o.both()
    );
    assert!(
        o.both().contains("not-reproduced"),
        "refusal must name the outcome not-reproduced: {}",
        o.both()
    );
    assert_eq!(
        stored_cmd(&f, &id),
        OLD_CMD,
        "stored repro must be unchanged"
    );
    assert_eq!(f.store_bytes(), before, "store bytes must be unchanged");
}

#[test]
fn zero_tests_ran_command_is_refused_as_undetermined_and_store_unchanged() {
    let (f, id) = fixture("er-zero", "broken");
    let before = f.store_bytes();
    let o = f.run(&["edit", &id, "--repro-test", NEW_ZERO]);
    assert_ne!(
        o.code,
        0,
        "a repro run that executed zero tests (undetermined) must be refused: {}",
        o.both()
    );
    assert!(
        o.both().contains("undetermined"),
        "refusal must name the outcome undetermined: {}",
        o.both()
    );
    assert_eq!(
        stored_cmd(&f, &id),
        OLD_CMD,
        "stored repro must be unchanged"
    );
    assert_eq!(f.store_bytes(), before, "store bytes must be unchanged");
}

#[test]
fn non_pending_task_repro_edit_is_refused_and_store_unchanged() {
    let (f, id) = fixture("er-done", "broken");
    let c = f.run(&["cancel", &id, "--reason", "closing for the test"]);
    assert_eq!(c.code, 0, "setup cancel failed: {}", c.both());
    assert_eq!(f.status(&id), "cancelled");
    let before = f.store_bytes();
    let o = f.run(&["edit", &id, "--repro-test", NEW_IGN]);
    assert_ne!(
        o.code,
        0,
        "editing the repro of a non-pending (cancelled) task must be refused: {}",
        o.both()
    );
    assert!(
        !o.both().contains("unexpected argument"),
        "refusal must come from the edit logic (task not pending), not from clap rejecting the flag: {}",
        o.both()
    );
    assert_eq!(
        stored_cmd(&f, &id),
        OLD_CMD,
        "stored repro of a closed task must be unchanged"
    );
    assert_eq!(f.store_bytes(), before, "store bytes must be unchanged");
}

#[test]
fn refused_repro_does_not_apply_other_flags_atomic() {
    // The repro command would pass => refused; --title must not be written.
    let (f, id) = fixture("er-atomic", "fixed");
    let before = f.store_bytes();
    let o = f.run(&["edit", &id, "--title", "NEW TITLE", "--repro-test", NEW_IGN]);
    assert_ne!(
        o.code,
        0,
        "combined edit whose repro is refused must fail: {}",
        o.both()
    );
    assert!(
        o.both().contains("not-reproduced"),
        "refusal must name the outcome not-reproduced (not a clap flag rejection): {}",
        o.both()
    );
    assert_eq!(
        f.row(&id)["title"],
        "edit me",
        "title must be unchanged when the combined edit's repro is refused (atomic)"
    );
    assert_eq!(f.store_bytes(), before, "store bytes must be unchanged");
}

/// CONTROL: the pre-existing flags keep working.
#[test]
fn control_edit_title_still_works() {
    let (f, id) = fixture("er-ctl", "broken");
    let o = f.run(&["edit", &id, "--title", "x"]);
    assert_eq!(o.code, 0, "edit --title must still work: {}", o.both());
    assert_eq!(f.row(&id)["title"], "x");
    assert_eq!(
        stored_cmd(&f, &id),
        OLD_CMD,
        "title edit must not touch repro"
    );
}
