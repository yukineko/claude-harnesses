#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Audit B item 4 (memory `scope-and-standing-rulings`, "Check ticket rulings
//! before dispatch"). A ticket the user ruled against (見送り / defer) must be
//! excluded by CODE, not by an agent remembering to grep its notes:
//! `backlog defer <id> --ruling "<quote>"` records the ruling, and
//! `next --claim` never hands that ticket out afterwards.
//!
//! Drives the real built binary (common::Fixture: isolated HOME, linked git
//! worktree, committed repro scripts).

mod common;

use common::Fixture;

#[test]
#[ignore = "backlog b3f5e3af: open defect, remove ignore when fixed"]
fn a_deferred_ticket_is_never_handed_out_by_next_claim() {
    let f = Fixture::new("auditb4");
    let ruled = f.add("ruled against by the user");
    let free = f.add("free to work");

    let d = f.run(&[
        "defer",
        &ruled,
        "--ruling",
        "user 2026-10-14: スクリプト実行は全部見送り",
    ]);
    assert_eq!(
        d.code,
        0,
        "`backlog defer <id> --ruling <quote>` must record the ruling: {}",
        d.both()
    );

    // CONTROL: the queue is not empty — the un-ruled ticket is handed out, so
    // "not returned" below cannot pass on an empty or refusing queue.
    let c1 = f.run(&["next", "--claim"]);
    assert_eq!(c1.code, 0, "next --claim must succeed: {}", c1.both());
    assert!(
        c1.stdout.contains(&free),
        "control: next --claim must hand out the un-ruled ticket {free}: {}",
        c1.both()
    );
    assert!(
        !c1.stdout.contains(&ruled),
        "next --claim handed out the deferred ticket {ruled}: {}",
        c1.both()
    );

    let c2 = f.run(&["next", "--claim"]);
    assert!(
        !c2.stdout.contains(&ruled),
        "next --claim handed out the deferred ticket {ruled} once the rest was claimed: {}",
        c2.both()
    );
}
