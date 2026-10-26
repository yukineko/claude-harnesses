// Integration test: unwrap/expect/panic are fine here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! GitHub issue #1347: `backlog done` classifies a behavioural RED as a build
//! failure whenever the RED output mentions "could not compile" anywhere.
//!
//! `classify_red` (crates/backlog/src/evidence.rs) checks the build-failure
//! markers BEFORE it looks for `test result: FAILED`. A realistic
//! `cargo test --workspace --no-fail-fast` RED run can carry both: one package
//! fails to compile (an unrelated crate), while the package under test BUILDS,
//! runs, and fails its assertion (`test result: FAILED`). That run did show a
//! test that ran and failed, so it is a behavioural RED and `done` must accept
//! it.
//!
//! Written by the test author only (CLAUDE.md section 2(a)); no fix here.
mod common;
use common::*;

/// Fake `cargo`, chosen by the `marker` file in the checkout under test:
///   mixed: an UNRELATED package fails to compile, and the package under test
///          runs one test that FAILS (`test result: FAILED`), exit 101
///   fixed: `test result: ok. 1 passed`, exit 0
const MIXED_CARGO: &str = r#"#!/bin/bash
m="$(cat marker 2>/dev/null)"
case "$m" in
  mixed)
    echo "   Compiling other-crate v0.1.0"
    echo "error[E0425]: cannot find value \`nope\` in this scope"
    echo "error: could not compile \`other-crate\` (lib test) due to 1 previous error"
    echo "     Running tests/subject.rs (target/debug/deps/subject-0123)"
    echo "running 1 test"
    echo "test subject_behaves ... FAILED"
    echo "failures:"
    echo "---- subject_behaves stdout ----"
    echo "assertion \`left == right\` failed: impl is still broken"
    echo "test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out"
    exit 101;;
  fixed)
    echo "running 1 test"
    echo "test subject_behaves ... ok"
    echo "test result: ok. 1 passed; 0 failed; 0 ignored"
    exit 0;;
  *) echo "no marker"; exit 2;;
esac
"#;

#[test]
fn red_with_test_result_failed_and_unrelated_could_not_compile_is_behavioural() {
    let f = Fixture::new("gh1347");
    f.write_exec_shim("cargo", MIXED_CARGO);

    // Rev A: the bug, and the RED run shows a genuine `test result: FAILED`
    // alongside an unrelated crate's "could not compile".
    f.write("src/impl.txt", "broken\n");
    f.write("marker", "mixed");
    let red = f.commit_paths(&["src/impl.txt", "marker"], "buggy impl");
    // Rev B: the fix; the same command is GREEN.
    f.write("src/impl.txt", "fixed\n");
    f.write("marker", "fixed");
    f.write_script("tests/check.sh", "exit 0");
    f.commit_paths(&["src/impl.txt", "marker", "tests/check.sh"], "fix + test");

    let id = f.add("gh1347 subject");
    let o = f.run(&["done", &id, "--test", "cargo test -p x", "--red-rev", &red]);
    // The literal build marker is redacted from this test's own failure
    // message: the message is part of THIS test's RED output, and an
    // unredacted copy would make `backlog add --repro-test` hit issue #1347
    // itself and refuse this repro as a build failure.
    let shown = o.both().replace("could not compile", "could-not-compile");
    assert_eq!(
        o.code,
        0,
        "a RED run with `test result: FAILED` is a behavioural RED even if an unrelated \
         package's compile-error summary appears in the same output; `done` must accept it: {shown}"
    );
    assert_eq!(f.status(&id), "done", "{shown}");
}
