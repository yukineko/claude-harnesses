#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED: a commit that only FILES a RED test, but whose subject carries the
//! plain `(backlog <id>)` marker, makes `backlog next --claim` treat `<id>` as
//! "likely fixed" and never hand it out -- although nothing was fixed.
//!
//! Real history this mirrors (main, observed 2026-11-22 at rev 2a83942c0):
//!   - `4458932ac` "test(backlog): tag zero-test RED with RED(f20d620c) (backlog f20d620c)"
//!   - `f088e7acb` "Merge session-62e92d16: RED for check-version-bumped
//!     working-tree judging (backlog d1d85c66)" -- d1d85c66 is still pending and
//!     its RED script is unchanged since its RED rev, yet `next` skips it.
//!
//! The fixture commit adds ONLY a test file whose test is
//! `#[ignore = "RED(<X>): ..."]` (no product change), with a subject of the
//! same shape as 4458932ac. Passing means: `backlog next --claim` exits 0 and
//! hands out X (p0, the highest-priority pending task; its repro still
//! reproduces).
//!
//! Does NOT prove: which side should change (the landed-fix detector in
//! `evidence::fixed_task_ids`, or the commit-subject convention for RED-only
//! commits, e.g. `(backlog <id> RED repro)`, which is already recognised as a
//! non-fix); plain `next` (same detector, not exercised here); merge commits.

mod common;

#[test]
#[ignore = "RED(363ad7da): RED-only commit with a plain (backlog <id>) subject hides the task from next --claim"]
fn red_only_commit_with_plain_marker_does_not_hide_task_from_claim() {
    let fx = common::Fixture::new("red-only-subject");
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
    fx.write(
        "tests/alpha_red.rs",
        &format!(
            "#[test]\n#[ignore = \"RED({x}): alpha parser drops trailing newline\"]\nfn alpha() {{ panic!(\"red\"); }}\n"
        ),
    );
    fx.commit_paths(
        &["tests/alpha_red.rs"],
        &format!("test(alpha): tag zero-test RED with RED({x}) (backlog {x})"),
    );
    let o = fx.run(&["next", "--claim"]);
    assert_eq!(
        o.code,
        0,
        "`backlog next --claim` should still dispatch X ({x}) after a commit that only filed its RED test; got exit {}: {}",
        o.code,
        o.both()
    );
    assert!(
        o.stdout.contains(&x),
        "a RED-only commit subject `(backlog {x})` was treated as a fix: `backlog next --claim` did not hand out X ({x}). stdout: {} stderr: {}",
        o.stdout,
        o.stderr
    );
}
