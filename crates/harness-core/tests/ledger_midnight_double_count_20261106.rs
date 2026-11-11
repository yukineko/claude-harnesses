//! RED repro for GitHub issue #1553 (reading (a), defect: the issue's own
//! "Distinguish" paragraph notes the sum over days exceeding real spend is
//! justified by no reading). budgetguard's Stop hook calls
//! `ledger.record(session_id, today, session_usd)` with the session's
//! CUMULATIVE cost. A session that crosses local midnight therefore keeps its
//! pre-midnight part under yesterday AND its full cumulative cost under today.
//! Observed: session 4a9031bf had 24.18 under 2026-11-05 and 49.06 (cumulative)
//! under 2026-11-06.

use harness_core::ledger::Ledger;

const EPS: f64 = 1e-9;

fn total_across_days(l: &Ledger) -> f64 {
    l.days.keys().map(|d| l.day_total(d)).sum()
}

/// Control (passes today): a session within one day is counted once.
#[test]
fn control_single_day_session_counted_once() {
    let mut l = Ledger::default();
    l.record("s", "2026-11-06", 10.0);
    l.record("s", "2026-11-06", 30.0);
    assert!((total_across_days(&l) - 30.0).abs() < EPS);
}

#[test]
fn session_crossing_midnight_total_across_days_equals_cumulative_cost() {
    let mut l = Ledger::default();
    // Stop before midnight: cumulative 24.18 recorded under yesterday.
    l.record("4a9031bf", "2026-11-05", 24.18);
    // Stop after midnight: cumulative 49.06 (includes the 24.18 above).
    l.record("4a9031bf", "2026-11-06", 49.06);
    let total = total_across_days(&l);
    assert!(
        (total - 49.06).abs() < 1e-6,
        "sum over days = {total}, but the session's real cumulative spend is 49.06 \
         (yesterday {}, today {})",
        l.day_total("2026-11-05"),
        l.day_total("2026-11-06")
    );
}
