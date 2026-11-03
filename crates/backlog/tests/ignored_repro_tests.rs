//! `#[ignore]`d RED repro tests as evidence (user ruling: committed intentional
//! RED repros carry `#[ignore = "backlog <id>: ..."]`, so `backlog add
//! --repro-test` / `backlog done --test` must be able to run them with
//! `-- --ignored` / `-- --include-ignored` and judge by the REAL outcome):
//!   * an ignored test that FAILS under `--ignored`  = reproduced;
//!   * a run that executes ZERO tests (filter matches nothing, or the ignored
//!     test run without `--ignored`: "0 passed; 1 ignored") is UNDETERMINED,
//!     never "reproduced", never "fixed", and not the clean "not-reproduced"
//!     either (exit 0 with nothing run proves nothing).
//!
//! Independent test writer (CLAUDE.md §2(a)). The fake `cargo` below emulates
//! libtest's treatment of one `#[ignore]`d test named `repro_ignored_one`,
//! whose pass/fail is decided by the `marker` file in the checkout cwd
//! (`broken` -> the test fails, anything else -> it passes), exactly as the
//! shared fixture's fake cargo does for non-ignored tests.
mod common;
use common::*;

const FAKE_CARGO_IGNORED: &str = r#"#!/bin/bash
m="$(cat marker 2>/dev/null)"
ignored=0
filter=""
after_dd=0
skip=0
for a in "$@"; do
  if [ "$skip" = 1 ]; then skip=0; continue; fi
  if [ "$after_dd" = 0 ]; then
    case "$a" in
      --) after_dd=1;;
      -p|--package|--test|--bin|--lib) skip=1;;
      -*|test) ;;
      *) filter="$a";;
    esac
  else
    case "$a" in
      --ignored|--include-ignored) ignored=1;;
      --exact|--nocapture|--test-threads=*) ;;
      -*) ;;
      *) filter="$a";;
    esac
  fi
done
if [ -n "$filter" ] && [[ "repro_ignored_one" != *"$filter"* ]]; then
  echo "running 0 tests"
  echo "test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 1 filtered out"
  exit 0
fi
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

const IGN: &str = "cargo test -p x -- --ignored";
const IGN_INCLUDE: &str = "cargo test -p x -- --include-ignored";
const IGN_EXACT: &str = "cargo test -p x repro_ignored_one -- --ignored --exact";

/// Fixture whose HEAD marker is `broken` (the bug is present at HEAD).
fn broken_fixture(tag: &str) -> Fixture {
    let f = Fixture::new(tag);
    f.write_exec_shim("cargo", FAKE_CARGO_IGNORED);
    f.write("marker", "broken");
    f.commit_paths(&["marker"], "bug present");
    f
}

fn add_repro(f: &Fixture, cmd: &str) -> Out {
    let p = f.project();
    f.run(&["add", "--title", cmd, "--project", &p, "--repro-test", cmd])
}

fn assert_not_filed(f: &Fixture, o: &Out, what: &str) {
    assert_eq!(o.code, 2, "{what}: must be refused (exit 2): {}", o.both());
    assert!(
        !o.stdout.lines().any(|l| l.starts_with("added: ")),
        "{what}: must not be filed: {}",
        o.stdout
    );
    assert!(f.list_all().is_empty(), "{what}: no row: {:?}", f.list_all());
}

// ---- add: --repro-test ------------------------------------------------------

#[test]
fn ignored_failing_test_with_dash_dash_ignored_is_reproduced() {
    let f = broken_fixture("ign-yes");
    let (o, id) = {
        let p = f.project();
        let o = f.run(&["add", "--title", "t", "--project", &p, "--repro-test", IGN]);
        let id = o
            .stdout
            .lines()
            .find_map(|l| l.strip_prefix("added: "))
            .map(|s| s.trim().to_string());
        (o, id)
    };
    assert_eq!(o.code, 0, "ignored failing test is reproduced: {}", o.both());
    let id = id.expect("added: line");
    assert_eq!(f.status(&id), "pending");
    assert!(
        f.row(&id).to_string().contains("\"reproduced\""),
        "{}",
        f.row(&id)
    );
}

#[test]
fn include_ignored_and_exact_filter_forms_are_reproduced() {
    for cmd in [IGN_INCLUDE, IGN_EXACT] {
        let f = broken_fixture("ign-forms");
        let o = add_repro(&f, cmd);
        assert_eq!(o.code, 0, "{cmd:?} is reproduced: {}", o.both());
        assert_eq!(f.list_all().len(), 1, "{cmd:?} filed once");
    }
}

/// CONTROL: an ignored test that PASSES under `--ignored` is not-reproduced
/// (a real, clean exit 0 with a test run) and must never be filed.
#[test]
fn control_ignored_passing_test_is_not_reproduced_and_not_filed() {
    let f = Fixture::new("ign-pass");
    f.write_exec_shim("cargo", FAKE_CARGO_IGNORED);
    f.write("marker", "fixed");
    f.commit_paths(&["marker"], "bug absent");
    let o = add_repro(&f, IGN);
    assert_not_filed(&f, &o, "passing ignored test");
    assert!(
        o.stderr.contains("repro outcome not-reproduced"),
        "a test that ran and passed is not-reproduced: {}",
        o.both()
    );
}

/// The ignored test run WITHOUT `--ignored` executes zero tests ("0 passed;
/// 1 ignored"). Refused, and classified undetermined: nothing ran, so it is
/// not a clean "not-reproduced" observation either.
#[test]
fn ignored_test_without_ignored_flag_is_undetermined() {
    let f = broken_fixture("ign-noflag");
    let o = add_repro(&f, "cargo test -p x");
    assert_not_filed(&f, &o, "ignored test run without --ignored");
    assert!(
        o.stderr.contains("repro outcome undetermined"),
        "0 tests run (1 ignored) must be undetermined, not not-reproduced: {}",
        o.both()
    );
}

#[test]
fn filter_matching_zero_tests_is_undetermined() {
    let f = broken_fixture("ign-nofilter");
    let o = add_repro(&f, "cargo test -p x no_such_test -- --ignored");
    assert_not_filed(&f, &o, "filter matching nothing");
    assert!(
        o.stderr.contains("repro outcome undetermined"),
        "0 tests run (filtered out) must be undetermined, not not-reproduced: {}",
        o.both()
    );
}

// ---- done --test ------------------------------------------------------------

/// Scenario: rev A = bug (marker broken), rev B = fix (marker fixed).
fn fixed_pair(tag: &str) -> (Fixture, String) {
    let f = Fixture::new(tag);
    f.write_exec_shim("cargo", FAKE_CARGO_IGNORED);
    f.write("marker", "broken");
    let red = f.commit_paths(&["marker"], "bug present");
    f.write("marker", "fixed");
    f.commit_paths(&["marker"], "fix");
    (f, red)
}

fn done(f: &Fixture, id: &str, cmd: &str, red: &str) -> Out {
    f.run(&["done", id, "--test", cmd, "--red-rev", red])
}

#[test]
fn done_with_ignored_test_now_passing_is_accepted() {
    let (f, red) = fixed_pair("done-ign-ok");
    let id = f.add("t");
    let o = done(&f, &id, IGN, &red);
    assert_eq!(o.code, 0, "ignored test RED->GREEN closes: {}", o.both());
    assert_eq!(f.status(&id), "done");
}

/// CONTROL: the ignored test still fails at HEAD -> refused, row unchanged.
#[test]
fn control_done_with_ignored_test_still_failing_is_refused() {
    let f = broken_fixture("done-ign-fail");
    let red = f.head();
    f.write("README.txt", "y\n");
    f.commit_paths(&["README.txt"], "unrelated change");
    let id = f.add("t");
    let o = done(&f, &id, IGN, &red);
    assert_refused_unchanged(&f, &id, "pending", &o, "ignored test still fails at HEAD");
}

/// CONTROL: GREEN run executes zero tests ("0 passed; 1 ignored") -> refused.
#[test]
fn control_done_with_zero_tests_run_is_refused() {
    let (f, red) = fixed_pair("done-ign-zero");
    let id = f.add("t");
    for cmd in [
        "cargo test -p x",
        "cargo test -p x no_such_test -- --ignored",
    ] {
        let o = done(&f, &id, cmd, &red);
        assert_refused_unchanged(&f, &id, "pending", &o, cmd);
    }
}
