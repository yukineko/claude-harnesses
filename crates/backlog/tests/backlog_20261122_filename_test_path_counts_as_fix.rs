#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED: after the fix for 363ad7da (merged in accf04160), a `(backlog <id>)`
//! commit counts as a landed fix only when it changes product code -- but
//! `evidence::is_test_path` recognises a test file ONLY by a `tests`/`test`
//! DIRECTORY component. A commit that adds nothing but a test file identified
//! by its FILENAME (`scripts/test_alpha.py`, `src/alpha_test.rs`) is read as a
//! product change, so `backlog next --claim` treats `<id>` as fixed and never
//! hands it out.
//!
//! Real history this mirrors (main, observed 2026-11-22 at rev 6ae161ee7):
//!   `f088e7acb` "Merge session-62e92d16: RED for check-version-bumped
//!   working-tree judging (backlog d1d85c66)" -- its first-parent diff is
//!   `scripts/tests/...sh` + `scripts/test_check_plugin_rollout.py`; the second
//!   path has no `tests`/`test` directory component.
//!
//! Passing means: `backlog next --claim` exits 0 and hands out X (p0, the
//! highest-priority pending task; its repro still reproduces) after a commit
//! that only added a filename-identified test file.
//!
//! Does NOT prove: which filename conventions the detector should recognise
//! (`test_*.py`, `*_test.py`, `*_test.rs`, `*.test.ts`, ...), nor whether the
//! right fix is the path classifier or the commit-subject convention
//! (`(backlog <id> RED repro)` is already recognised as a non-fix); plain
//! `next` (same detector, not exercised here); merge commits.

mod common;

fn assert_claim_hands_out_x_after_test_only_commit(tag: &str, test_path: &str, body: &str) {
    let fx = common::Fixture::new(tag);
    let (ox, x) = fx.add_extra(
        "alpha parser drops trailing newline",
        &["--priority", "p0", "--repro-test", common::REPRO_YES],
    );
    let (oy, y) = fx.add_extra(
        "zulu renderer misaligns footer",
        &["--priority", "p2", "--repro-test", common::REPRO_YES],
    );
    let x = x.unwrap_or_else(|| panic!("setup: X not filed: {}", ox.both()));
    let _y = y.unwrap_or_else(|| panic!("setup: Y not filed: {}", oy.both()));
    fx.write(test_path, &body.replace("{x}", &x));
    fx.commit_paths(
        &[test_path],
        &format!("test(alpha): RED repro script for alpha (backlog {x})"),
    );
    let o = fx.run(&["next", "--claim"]);
    assert_eq!(
        o.code,
        0,
        "`backlog next --claim` should still dispatch X ({x}) after a commit that only added the test file {test_path}; got exit {}: {}",
        o.code,
        o.both()
    );
    assert!(
        o.stdout.contains(&x),
        "a commit adding only the test file {test_path} with subject `(backlog {x})` was treated as a fix: `backlog next --claim` did not hand out X ({x}). stdout: {} stderr: {}",
        o.stdout,
        o.stderr
    );
}

#[test]
fn python_test_file_by_filename_is_not_a_fix() {
    assert_claim_hands_out_x_after_test_only_commit(
        "fname-test-py",
        "scripts/test_alpha.py",
        "# RED({x})\ndef test_alpha():\n    assert False, 'red'\n",
    );
}

#[test]
fn rust_test_file_by_filename_is_not_a_fix() {
    assert_claim_hands_out_x_after_test_only_commit(
        "fname-test-rs",
        "src/alpha_test.rs",
        "#[test]\n#[ignore = \"RED({x}): alpha\"]\nfn alpha() { panic!(\"red\"); }\n",
    );
}
