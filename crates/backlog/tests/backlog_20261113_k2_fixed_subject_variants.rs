#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro (audit 2026-11-13 K2): `evidence::fixed_task_ids` takes everything
//! between `(backlog ` and the next `)` as the task id, so commit subjects
//! `fix: x (backlog <id> wave 2)` and `fix: x (backlog <id>; y)` yield ids
//! `"<id> wave 2"` / `"<id>; y"` that never equal `<id>`: the fix landed but
//! `backlog next` still dispatches the task.
//!
//! Fixture mirrors backlog_scout_20261112_f2_dispatch_fixed_pending.rs: two
//! pending tasks, X (p0) and control Y (p2); X's fix commit lands. Accepted
//! fixed behaviour: `next` returns Y, or exits non-zero naming X as fixed.
//! Measurement point: 09dd0c8c, 2026-11-13.
//!
//! Does NOT prove: which suffix grammar is the right one (only that the id
//! token directly after `(backlog ` identifies the task in these two
//! variants), nor `next --claim`, nor ids that are prefixes of one another.

mod common;

fn setup(tag: &str, marker: impl Fn(&str) -> String) -> (common::Fixture, String, String) {
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
    let y = y.unwrap_or_else(|| panic!("setup: Y not filed: {}", oy.both()));
    fx.write("src/fix.txt", "fixed\n");
    fx.commit_paths(
        &["src/fix.txt"],
        &format!("fix: alpha parser drops trailing newline {}", marker(&x)),
    );
    (fx, x, y)
}

fn assert_not_dispatched(o: &common::Out, x: &str, y: &str) {
    if o.code == 0 {
        assert!(
            !o.stdout.contains(x),
            "`backlog next` handed out X ({x}) although its fix commit names it; expected Y ({y}). stdout: {}",
            o.stdout
        );
        assert!(
            o.stdout.contains(y),
            "`backlog next` exited 0 without X but did not hand out Y ({y}). stdout: {}",
            o.stdout
        );
    } else {
        assert!(
            o.both().contains(x),
            "`backlog next` exited {} without naming X ({x}) as already fixed: {}",
            o.code,
            o.both()
        );
    }
}

#[test]
fn control_plain_subject_is_recognized() {
    let (fx, x, y) = setup("k2-ctl", |x| format!("(backlog {x})"));
    assert_not_dispatched(&fx.run(&["next"]), &x, &y);
}

#[test]
#[ignore = "RED: K2 — remove the ignore when fixed"]
fn subject_with_wave_suffix_is_recognized() {
    let (fx, x, y) = setup("k2-wave", |x| format!("(backlog {x} wave 2)"));
    assert_not_dispatched(&fx.run(&["next"]), &x, &y);
}

#[test]
#[ignore = "RED: K2 — remove the ignore when fixed"]
fn subject_with_semicolon_suffix_is_recognized() {
    let (fx, x, y) = setup("k2-semi", |x| format!("(backlog {x}; y)"));
    assert_not_dispatched(&fx.run(&["next"]), &x, &y);
}
