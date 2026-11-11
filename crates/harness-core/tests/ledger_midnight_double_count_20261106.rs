//! RED repro (backlog): a session that crosses local midnight is counted twice.
//!
//! budgetguard records the session's CUMULATIVE cost (whole transcript) under
//! today's date key (`gate.rs`: `ledger.record(session_id, today, session_usd)`).
//! A session alive across midnight therefore keeps its pre-midnight spend in
//! yesterday's entry AND carries it again inside today's cumulative value.
//! Observed 2026-11-06 in ~/.budgetguard/state/ledger.json: session 4a9031bf
//! has 24.18 on 2026-11-05 and 49.06 (cumulative) on 2026-11-06; 2 sessions
//! appear on both days. The inflated day totals also feed derive_limits (Tukey
//! fence over past days), which raises the derived limits — a loosening.
//!
//! Invariant pinned: the sum of day totals over all days equals what was
//! actually spent (the session's final cumulative cost), and today's total
//! holds only today's spend.

use harness_core::ledger::Ledger;

#[test]
#[ignore = "backlog: midnight double count (RED repro)"]
fn session_crossing_midnight_is_not_double_counted() {
    let mut l = Ledger::default();
    // 23:50 on day 1: the session has spent 10 so far.
    l.record("s", "2026-11-05", 10.0);
    // 00:10 on day 2: cumulative spend of the same session is 15.
    let day2 = l.record("s", "2026-11-06", 15.0);

    let all: f64 = l.days.values().map(|d| d.total()).sum();
    assert!(
        (all - 15.0).abs() < 1e-9,
        "sum over days must equal real spend 15, got {all} (pre-midnight 10 counted twice)"
    );
    assert!(
        (day2 - 5.0).abs() < 1e-9,
        "day 2 must hold only day-2 spend 5, got {day2}"
    );
}

#[test]
fn control_single_day_session_is_unchanged() {
    let mut l = Ledger::default();
    l.record("s", "2026-11-06", 10.0);
    let t = l.record("s", "2026-11-06", 15.0);
    assert!((t - 15.0).abs() < 1e-9, "same-day updates replace, got {t}");
}
