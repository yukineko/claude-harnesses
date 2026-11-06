#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro (scout 2026-11-05): `backlog list --json` rewrites `status` to
//! "deferred" for ANY row whose `defer_until` is in the future, including rows
//! that are already terminal (`done`). `Task::is_deferred` (task.rs) does not
//! look at status, and main.rs applies it unconditionally in the JSON arm.
//! overwatch counts `status == "deferred"`, so a closed task is reported as
//! "on hold".

mod common;

#[test]
fn done_row_with_future_defer_until_keeps_status_done_in_json() {
    let fx = common::Fixture::new("json-defer-terminal");
    fx.plant_legacy_done("aaaa1111", "closed but carries a stale defer_until", "done");
    let p = fx.done_path();
    let body = std::fs::read_to_string(&p).unwrap();
    // Year 2100: unambiguously in the future.
    let patched = body.replace("notes = \"\"", "notes = \"\"\ndefer_until = 4102444800");
    assert_ne!(body, patched, "fixture patch applied");
    std::fs::write(&p, patched).unwrap();

    let row = fx.row("aaaa1111");
    assert_eq!(
        row["status"], "done",
        "a done row must stay `done` in the JSON feed; got {:?} (row: {row})",
        row["status"]
    );
}
