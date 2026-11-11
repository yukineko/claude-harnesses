#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro (scout 2026-11-12 F2): `backlog next` (and `next --claim`) hands
//! out a `pending` task whose fix commit -- a HEAD ancestor whose message names
//! the task id (`fix: <title> (backlog <id>)`) -- already landed. A driver
//! re-implements work that is already fixed.
//!
//! Fixture: isolated HOME + linked-worktree git repo (tests/common), two
//! pending tasks: X (p0) and the lower-priority control Y (p2).
//!
//! Accepted fixed behaviour: `next` returns Y, or exits non-zero with a
//! diagnostic naming X as already fixed. It must NOT hand out X.

mod common;

fn setup(tag: &str) -> (common::Fixture, String, String) {
    let fx = common::Fixture::new(tag);
    let (ox, x) = fx.add_extra(
        "alpha parser drops trailing newline",
        &["--priority", "p0", "--repro-test", common::REPRO_YES],
    );
    let (oy, y) = fx.add_extra(
        "zulu renderer misaligns footer",
        &["--priority", "p2", "--repro-test", common::REPRO_YES],
    );
    (
        fx,
        x.unwrap_or_else(|| panic!("setup: X not filed: {}", ox.both())),
        y.unwrap_or_else(|| panic!("setup: Y not filed: {}", oy.both())),
    )
}

fn land_fix(fx: &common::Fixture, x: &str) {
    fx.write("src/fix.txt", "fixed\n");
    fx.commit_paths(
        &["src/fix.txt"],
        &format!("fix: alpha parser drops trailing newline (backlog {x})"),
    );
}

fn assert_not_dispatched(o: &common::Out, x: &str, y: &str, what: &str) {
    if o.code == 0 {
        assert!(
            !o.stdout.contains(x),
            "{what} handed out X ({x}) although its fix commit `(backlog {x})` is an ancestor of HEAD; \
             expected Y ({y}). stdout: {}",
            o.stdout
        );
        assert!(
            o.stdout.contains(y),
            "{what} exited 0 without X but did not hand out Y ({y}) either; \
             emitting nothing is not a fix. stdout: {}",
            o.stdout
        );
    } else {
        assert!(
            o.both().contains(x),
            "{what} exited {} without naming X ({x}) as already fixed: {}",
            o.code,
            o.both()
        );
    }
}

#[test]
#[ignore = "scout 2026-11-12 F2: RED, open defect. Remove the ignore when fixed"]
fn next_does_not_dispatch_task_whose_fix_commit_is_in_head() {
    let (fx, x, y) = setup("f2-next");
    // Control: before the fix commit, X is the next task.
    let before = fx.run(&["next"]);
    assert_eq!(before.code, 0, "setup: {}", before.both());
    assert!(
        before.stdout.contains(&x),
        "setup control: next names X: {}",
        before.stdout
    );

    land_fix(&fx, &x);
    let after = fx.run(&["next"]);
    assert_not_dispatched(&after, &x, &y, "`backlog next`");
}

#[test]
#[ignore = "scout 2026-11-12 F2: RED, open defect. Remove the ignore when fixed"]
fn next_claim_does_not_dispatch_task_whose_fix_commit_is_in_head() {
    let (fx, x, y) = setup("f2-next-claim");
    land_fix(&fx, &x);
    let after = fx.run(&["next", "--claim"]);
    assert_not_dispatched(&after, &x, &y, "`backlog next --claim`");
    // State read back through the CLI (`list --all --json`).
    assert_eq!(
        fx.status(&x),
        "pending",
        "X ({x}) must stay pending, not be leased: {}",
        after.both()
    );
    if after.code == 0 {
        assert_eq!(fx.status(&y), "claimed", "Y ({y}) must be the claimed task");
    }
}
