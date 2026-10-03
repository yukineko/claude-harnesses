//! Contract tests for `backlog cancel ID --reason R` under the close-evidence
//! "discard" spec. User ruling: an item that cannot be proven is not a problem,
//! so discarding it must be cheap: NO TTY, NO ruling, NO test. The closure
//! evidence class recorded is `discard` with a non-empty `discard_reason`.
//!
//! Written by an independent test author (CLAUDE.md §2(a)), NOT the implementer.
//! Every refusal case asserts the store is byte-identical, and the success cases
//! are the positive controls showing the same fixture CAN be cancelled.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;
use common::*;

const REASON: &str = "unproven speculation, discard";

fn parse_file(p: &std::path::Path) -> toml::Value {
    let s = std::fs::read_to_string(p).unwrap_or_default();
    s.parse::<toml::Value>()
        .unwrap_or_else(|e| panic!("{} not valid toml ({e}): {s}", p.display()))
}

fn rows(v: &toml::Value) -> Vec<toml::Value> {
    v.get("task")
        .and_then(|t| t.as_array())
        .cloned()
        .unwrap_or_default()
}

fn find<'a>(rs: &'a [toml::Value], id: &str) -> Option<&'a toml::Value> {
    rs.iter()
        .find(|t| t.get("id").and_then(|x| x.as_str()) == Some(id))
}

/// Cancel `id` with REASON and assert the full discard contract.
fn assert_discards(f: &Fixture, id: &str) {
    let o = f.run(&["cancel", id, "--reason", REASON]);
    assert_eq!(o.code, 0, "cancel must succeed with no TTY: {}", o.both());
    assert!(
        o.stdout
            .lines()
            .any(|l| l.trim() == format!("cancelled: {id}")),
        "stdout needs a `cancelled: {id}` line: {:?}",
        o.stdout
    );
    assert_eq!(f.status(id), "cancelled");

    let live = rows(&parse_file(&f.tasks_path()));
    assert!(find(&live, id).is_none(), "row must leave tasks.toml");
    let done = rows(&parse_file(&f.done_path()));
    let row = find(&done, id).unwrap_or_else(|| panic!("row {id} not in tasks.done.toml"));
    let cl = row
        .get("closure")
        .and_then(|c| c.as_table())
        .unwrap_or_else(|| panic!("no [task.closure] table on cancelled row: {row}"));
    assert_eq!(
        cl.get("reason").and_then(|x| x.as_str()),
        Some("discard"),
        "{cl:?}"
    );
    assert_eq!(
        cl.get("discard_reason").and_then(|x| x.as_str()),
        Some(REASON),
        "{cl:?}"
    );
    for k in ["green", "red", "ruling", "duplicate_of", "doc_only_commit"] {
        assert!(
            cl.get(k).is_none(),
            "discard closure must not carry `{k}`: {cl:?}"
        );
    }
    assert!(
        row.get("notes")
            .and_then(|x| x.as_str())
            .is_some_and(|n| n.contains(REASON)),
        "notes must contain the reason: {row}"
    );
    assert!(row.get("defer_until").is_none(), "{row}");
    assert!(cl.get("defer_until").is_none(), "{cl:?}");
}

#[test]
fn cancel_pending_task_records_a_discard_closure_without_tty() {
    let f = Fixture::new("d1");
    let id = f.add("t");
    assert_eq!(f.status(&id), "pending");
    assert_discards(&f, &id);
}

#[test]
fn cancel_failed_task_records_a_discard_closure() {
    let f = Fixture::new("d1f");
    let id = f.add("t");
    let r = f.run(&["fail", &id, "--reason", "x"]);
    assert_eq!(r.code, 0, "{}", r.both());
    // `fail` defers the row (defer_until = now+2d); since d65da48d the JSON
    // feed reports that derived status as `deferred` (stored status: failed).
    assert_eq!(f.status(&id), "deferred");
    assert_discards(&f, &id);
}

#[test]
fn cancel_unconfirmed_task_records_a_discard_closure() {
    let f = Fixture::new("d1u");
    let (o, id) = f.add_extra("t", &[]);
    let id = id.unwrap_or_else(|| panic!("no added id: {}", o.both()));
    assert_eq!(f.status(&id), "unconfirmed");
    assert_discards(&f, &id);
}

#[test]
fn cancel_with_empty_or_whitespace_reason_is_refused() {
    let f = Fixture::new("d2");
    let id = f.add("t");
    for bad in ["", "   "] {
        let before = f.store_bytes();
        let o = f.run(&["cancel", &id, "--reason", bad]);
        assert_ne!(o.code, 0, "reason {bad:?} must be refused: {}", o.both());
        assert_eq!(f.store_bytes(), before, "store must be byte-identical");
        assert_eq!(f.status(&id), "pending");
    }
    // positive control: the same fixture is cancellable with a real reason.
    assert_discards(&f, &id);
}

#[test]
fn cancel_of_a_done_task_is_refused_and_closure_unchanged() {
    let f = Fixture::new("d3");
    let a = f.add("a");
    let b = f.add("b");
    let d = f.run(&["done", &a, "--duplicate-of", &b]);
    assert_eq!(d.code, 0, "{}", d.both());
    assert_eq!(f.status(&a), "done");
    let before = f.store_bytes();
    let o = f.run(&["cancel", &a, "--reason", REASON]);
    assert_ne!(
        o.code,
        0,
        "cancelling a done task must be refused: {}",
        o.both()
    );
    assert_eq!(f.status(&a), "done");
    assert_eq!(f.store_bytes(), before, "closure/store must be unchanged");
}

#[test]
fn cancel_of_a_needs_ruling_task_is_refused_mentioning_ruling() {
    let f = Fixture::new("d4");
    let id = f.add("t");
    let r = f.run(&[
        "ruling",
        "request",
        &id,
        "--kind",
        "judgment",
        "--rationale",
        "x",
    ]);
    assert_eq!(r.code, 0, "{}", r.both());
    assert_eq!(f.status(&id), "needs-ruling");
    let before = f.store_bytes();
    let o = f.run(&["cancel", &id, "--reason", REASON]);
    assert_ne!(o.code, 0, "{}", o.both());
    assert_eq!(f.status(&id), "needs-ruling");
    assert_eq!(f.store_bytes(), before);
    assert!(
        o.both().to_lowercase().contains("ruling"),
        "refusal must mention the pending ruling: {}",
        o.both()
    );
}

#[test]
fn second_cancel_of_a_cancelled_task_is_an_idempotent_noop() {
    let f = Fixture::new("d5");
    let id = f.add("t");
    assert_discards(&f, &id); // positive control: first cancel really lands
    let before = f.store_bytes();
    let o = f.run(&["cancel", &id, "--reason", "a different reason"]);
    assert_eq!(o.code, 0, "{}", o.both());
    assert_eq!(f.store_bytes(), before, "reason must not be appended twice");
    assert_eq!(f.status(&id), "cancelled");
}

#[test]
fn cancel_of_an_unknown_id_is_refused() {
    let f = Fixture::new("d6");
    let id = f.add("t");
    let before = f.store_bytes();
    let o = f.run(&["cancel", "zzzzzzzz", "--reason", REASON]);
    assert_ne!(o.code, 0, "unknown id must fail: {}", o.both());
    assert_eq!(f.store_bytes(), before);
    assert_eq!(f.status(&id), "pending");
}
