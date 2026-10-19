//! F2P when the fix lives in a TEST file (backlog ticket filed from this test;
//! first seen on overwatch 73e49761).
//!
//! `backlog done --test CMD --red-rev REV` runs RED at REV with HEAD's test
//! files overlaid (so a test added with the fix exists at REV). For a cargo
//! command that overlay is every test file of HEAD. When the fix itself is a
//! test file (the "subject" test, e.g. a `return` turned into a `panic!`), the
//! overlay copies the fix into the RED tree, so no command can ever be RED and
//! the ticket can never be closed honestly.
//!
//! `--red-overlay PATH` (repeatable) narrows the overlay to the named HEAD test
//! files (the repro side). It may only NARROW the default overlay, every path
//! must be a test file tracked at HEAD, the narrowed overlay is recorded in
//! `closure.red.overlay`, and a test that is GREEN at REV is still refused.
//!
//! Fixture: a fake `cargo` (first on PATH) decides by files in its CWD:
//!   tests/repro.txt absent         -> "0 passed", exit 0 (the repro does not exist)
//!   tests/subject.txt == "fixed"   -> "1 passed", exit 0
//!   otherwise                      -> "test result: FAILED", exit 101 (behavioural)
mod common;
use common::*;

const CMD: &str = "cargo test -p x --test subject --test repro";

const SUBJECT_CARGO: &str = r#"#!/bin/bash
if [ ! -f tests/repro.txt ]; then
  echo "running 0 tests"; echo "test result: ok. 0 passed; 0 failed; 0 ignored"; exit 0
fi
if [ "$(cat tests/subject.txt 2>/dev/null)" = fixed ]; then
  echo "running 1 test"; echo "test repro ... ok"; echo "test result: ok. 1 passed; 0 failed; 0 ignored"; exit 0
fi
echo "running 1 test"; echo "test repro ... FAILED"
echo "test result: FAILED. 0 passed; 1 failed; 0 ignored"; exit 101
"#;

/// rev A: the subject test file is buggy (`subject_at_red`); HEAD: the subject
/// test file is fixed and the repro test is added. Returns (red, head).
fn fix_in_test_file(f: &Fixture, subject_at_red: &str) -> (String, String) {
    f.write_exec_shim("cargo", SUBJECT_CARGO);
    f.write("tests/subject.txt", subject_at_red);
    let a = f.commit_paths(&["tests/subject.txt"], "subject test (buggy)");
    f.write("tests/subject.txt", "fixed");
    f.write("tests/repro.txt", "repro");
    f.write("src/impl.txt", "unrelated\n");
    let b = f.commit_paths(
        &["tests/subject.txt", "tests/repro.txt", "src/impl.txt"],
        "fix the subject test + add the repro",
    );
    (a, b)
}

#[test]
fn fix_in_a_test_file_closes_with_a_repro_only_overlay() {
    let f = Fixture::new("ovl-ok");
    let (red, head) = fix_in_test_file(&f, "buggy");
    let id = f.add("t");
    let o = f.run(&[
        "done",
        &id,
        "--test",
        CMD,
        "--red-rev",
        &red,
        "--red-overlay",
        "tests/repro.txt",
    ]);
    assert_eq!(
        o.code,
        0,
        "a fix that lives in the subject test file must be closable by overlaying only the \
         repro: {}",
        o.both()
    );
    assert_eq!(f.status(&id), "done");
    let c = f.row(&id)["closure"].clone();
    assert_eq!(c["red"]["rev"], red.as_str(), "{c}");
    assert_eq!(c["green"]["rev"], head.as_str(), "{c}");
    assert_eq!(
        c["red"]["overlay"],
        serde_json::json!(["tests/repro.txt"]),
        "the narrowed overlay must be recorded so the RED can be re-run the same way: {c}"
    );
}

#[test]
fn default_overlay_refusal_names_the_head_test_overlay() {
    let f = Fixture::new("ovl-default");
    let (red, _) = fix_in_test_file(&f, "buggy");
    let id = f.add("t");
    let o = f.run(&["done", &id, "--test", CMD, "--red-rev", &red]);
    assert_ne!(o.code, 0, "default overlay copies the fix in: {}", o.both());
    assert_eq!(f.status(&id), "pending");
    let msg = o.both();
    assert!(
        msg.contains("PASSES at --red-rev") && msg.contains("overlaid") && msg.contains("--red-overlay"),
        "the refusal must say the RED run had HEAD's test files overlaid and name --red-overlay: {msg}"
    );
}

#[test]
fn repro_only_overlay_still_refuses_a_test_green_at_red_rev() {
    // The subject is already fixed at the red rev: the bug is not there, so
    // narrowing the overlay must not manufacture a RED.
    let f = Fixture::new("ovl-green");
    let (red, _) = fix_in_test_file(&f, "fixed");
    let id = f.add("t");
    let o = f.run(&[
        "done",
        &id,
        "--test",
        CMD,
        "--red-rev",
        &red,
        "--red-overlay",
        "tests/repro.txt",
    ]);
    assert_ne!(o.code, 0, "{}", o.both());
    assert_eq!(f.status(&id), "pending");
    assert!(
        o.both().contains("PASSES at --red-rev"),
        "refused because the test passes at the red rev: {}",
        o.both()
    );
}

#[test]
fn red_overlay_paths_must_be_head_test_files_in_the_default_overlay() {
    let f = Fixture::new("ovl-bad");
    let (red, _) = fix_in_test_file(&f, "buggy");
    let id = f.add("t");
    for bad in ["src/impl.txt", "tests/nope.txt", "../tests/repro.txt", ""] {
        let o = f.run(&[
            "done",
            &id,
            "--test",
            CMD,
            "--red-rev",
            &red,
            "--red-overlay",
            bad,
        ]);
        assert_ne!(
            o.code,
            0,
            "--red-overlay {bad:?} must be refused: {}",
            o.both()
        );
        assert_eq!(f.status(&id), "pending", "{bad:?}");
        assert!(
            o.both().contains("--red-overlay") && o.both().contains("refused"),
            "--red-overlay {bad:?}: the refusal must name the flag: {}",
            o.both()
        );
    }
}

#[test]
fn red_overlay_cannot_widen_a_narrowed_pytest_overlay() {
    // A pytest argv naming tests/test_a.py overlays only that file by default;
    // --red-overlay may not add another test file to it.
    let f = Fixture::new("ovl-widen");
    f.write("tests/test_a.py", "a\n");
    f.write("tests/test_b.py", "b\n");
    let red = f.commit_paths(&["tests/test_a.py", "tests/test_b.py"], "two tests");
    let id = f.add("t");
    let o = f.run(&[
        "done",
        &id,
        "--test",
        "pytest tests/test_a.py",
        "--red-rev",
        &red,
        "--red-overlay",
        "tests/test_b.py",
    ]);
    assert_ne!(o.code, 0, "{}", o.both());
    assert_eq!(f.status(&id), "pending");
    assert!(
        o.both().contains("--red-overlay") && o.both().contains("default overlay"),
        "widening the overlay must be refused by name: {}",
        o.both()
    );
}
