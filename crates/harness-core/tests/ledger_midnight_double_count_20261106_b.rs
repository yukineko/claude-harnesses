//! GitHub #1553: a session that crosses local midnight is counted under both
//! day keys because `record` stores the session's CUMULATIVE cost per day.
//! Repro: cargo test -p harness-core --test ledger_midnight_double_count_20261106 -- --include-ignored

use harness_core::ledger::Ledger;

fn sum_over_days(l: &Ledger) -> f64 {
    l.days.values().map(|d| d.total()).sum()
}

/// CONTROL (passes): re-recording within one day overwrites, not accumulates.
#[test]
fn control_same_day_rerecord_is_idempotent() {
    let mut l = Ledger::default();
    l.record("s", "2026-11-05", 10.0);
    l.record("s", "2026-11-05", 24.18);
    assert!((l.day_total("2026-11-05") - 24.18).abs() < 1e-9);
}

/// CONTROL (passes): distinct sessions on distinct days sum normally.
#[test]
fn control_distinct_sessions_sum() {
    let mut l = Ledger::default();
    l.record("a", "2026-11-05", 3.0);
    l.record("b", "2026-11-06", 4.0);
    assert!((sum_over_days(&l) - 7.0).abs() < 1e-9);
}

/// RED: one session whose cumulative cost is 49.06 (24.18 spent before
/// midnight) must total 49.06 across all days, not 73.24.
#[test]
fn session_crossing_midnight_is_not_double_counted() {
    let mut l = Ledger::default();
    l.record("4a9031bf", "2026-11-05", 24.18);
    l.record("4a9031bf", "2026-11-06", 49.06);
    let total = sum_over_days(&l);
    assert!(
        (total - 49.06).abs() < 1e-9,
        "sum over days = {total}, real cumulative spend = 49.06 (day totals: 11-05={}, 11-06={})",
        l.day_total("2026-11-05"),
        l.day_total("2026-11-06")
    );
}
