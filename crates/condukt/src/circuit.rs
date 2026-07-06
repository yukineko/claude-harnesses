//! CIRCUIT-BREAKER gate: deterministic "keep looping vs trip" decision core.
//!
//! An autonomous run loop can burn cost or spin forever when something has gone
//! wrong: a task fails over and over, the budget blows past its cap, or the run
//! stops making progress and idles. This module is the deterministic core
//! consulted by `condukt circuit check` on each loop iteration: given a handful
//! of already-gathered signals (a consecutive-failure streak, a
//! budget-over-cap flag, and an idle duration) it decides whether to `Continue`
//! the loop or `Trip` the breaker with a stable reason.
//!
//! Purity guarantee (mirrors [`crate::run_policy::decide_run_policy`] and
//! [`crate::policy::decide`]): no filesystem, no `std::time`, no env, no LLM.
//! The caller gathers the signals; this function is a total, deterministic
//! function of its arguments and never panics. The caps are opt-out: a
//! `streak_cap` of 0 disables the streak condition, and an `idle_ttl_secs` of 0
//! disables the stall condition.

// The pure core lands ahead of its `condukt circuit check` subcommand consumer
// (a separate downstream task), so nothing outside the tests references these
// items yet. Allow dead_code until that wiring lands, matching how the decision
// core is developed and unit-tested independently of its CLI surface.
#![allow(dead_code)]

/// Why the circuit breaker tripped. Stable lowercase slugs (via [`as_str`]) are
/// journaled so downstream tooling can key off the reason.
///
/// [`as_str`]: CircuitReason::as_str
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CircuitReason {
    /// The consecutive-failure streak reached (or exceeded) its cap.
    FailureStreak,
    /// The run's budget went over its cap — the hardest stop (cost).
    BudgetOverCap,
    /// The run idled at least as long as its time-to-live without progress.
    Stall,
}

impl CircuitReason {
    /// A stable lowercase slug for journaling (`"failure_streak"` /
    /// `"budget_over_cap"` / `"stall"`).
    pub fn as_str(&self) -> &'static str {
        match self {
            CircuitReason::FailureStreak => "failure_streak",
            CircuitReason::BudgetOverCap => "budget_over_cap",
            CircuitReason::Stall => "stall",
        }
    }
}

/// The two-state verdict emitted by [`decide_circuit`]: keep looping, or trip
/// the breaker with a reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CircuitVerdict {
    /// No stop condition holds — keep looping.
    Continue,
    /// A stop condition holds — trip the breaker for the carried reason.
    Trip(CircuitReason),
}

/// Decide whether to keep looping or trip the circuit breaker.
///
/// Pure and deterministic: no LLM, no network, no filesystem, no clock. Never
/// panics. Conditions are checked in a fixed precedence (first match wins):
///
/// 1. `budget_over_cap` → `Trip(BudgetOverCap)` (cost is the hardest stop).
/// 2. else `streak_cap > 0 && streak >= streak_cap` → `Trip(FailureStreak)`.
/// 3. else `idle_ttl_secs > 0 && idle_secs >= idle_ttl_secs` → `Trip(Stall)`.
/// 4. otherwise → `Continue`.
///
/// A `streak_cap` of 0 disables the streak condition; an `idle_ttl_secs` of 0
/// disables the stall condition. These opt-outs keep the function total.
pub fn decide_circuit(
    streak: u32,
    streak_cap: u32,
    budget_over_cap: bool,
    idle_secs: i64,
    idle_ttl_secs: i64,
) -> CircuitVerdict {
    if budget_over_cap {
        CircuitVerdict::Trip(CircuitReason::BudgetOverCap)
    } else if streak_cap > 0 && streak >= streak_cap {
        CircuitVerdict::Trip(CircuitReason::FailureStreak)
    } else if idle_ttl_secs > 0 && idle_secs >= idle_ttl_secs {
        CircuitVerdict::Trip(CircuitReason::Stall)
    } else {
        CircuitVerdict::Continue
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── each trip reason reachable ─────────────────────────────────────────

    #[test]
    fn budget_over_cap_trips() {
        let v = decide_circuit(0, 5, true, 0, 60);
        assert_eq!(v, CircuitVerdict::Trip(CircuitReason::BudgetOverCap));
    }

    #[test]
    fn failure_streak_trips() {
        let v = decide_circuit(5, 5, false, 0, 60);
        assert_eq!(v, CircuitVerdict::Trip(CircuitReason::FailureStreak));
    }

    #[test]
    fn stall_trips() {
        let v = decide_circuit(0, 5, false, 60, 60);
        assert_eq!(v, CircuitVerdict::Trip(CircuitReason::Stall));
        // as_str slug is stable/journalable.
        if let CircuitVerdict::Trip(reason) = v {
            assert_eq!(reason.as_str(), "stall");
        } else {
            panic!("expected a Trip verdict");
        }
    }

    // ── the no-trip Continue case ──────────────────────────────────────────

    #[test]
    fn no_condition_holds_continues() {
        let v = decide_circuit(2, 5, false, 30, 60);
        assert_eq!(v, CircuitVerdict::Continue);
    }

    // ── boundary equality ──────────────────────────────────────────────────

    #[test]
    fn streak_equal_to_cap_trips() {
        assert_eq!(
            decide_circuit(5, 5, false, 0, 0),
            CircuitVerdict::Trip(CircuitReason::FailureStreak)
        );
    }

    #[test]
    fn streak_one_below_cap_does_not_trip() {
        assert_eq!(decide_circuit(4, 5, false, 0, 0), CircuitVerdict::Continue);
    }

    #[test]
    fn idle_equal_to_ttl_trips() {
        assert_eq!(
            decide_circuit(0, 0, false, 60, 60),
            CircuitVerdict::Trip(CircuitReason::Stall)
        );
    }

    #[test]
    fn idle_one_below_ttl_does_not_trip() {
        assert_eq!(
            decide_circuit(0, 0, false, 59, 60),
            CircuitVerdict::Continue
        );
    }

    // ── disabling semantics (cap of 0 opts the axis out) ───────────────────

    #[test]
    fn streak_cap_zero_never_trips_on_streak() {
        // A huge streak but streak_cap == 0 disables the condition.
        assert_eq!(
            decide_circuit(u32::MAX, 0, false, 0, 0),
            CircuitVerdict::Continue
        );
    }

    #[test]
    fn idle_ttl_zero_never_trips_on_stall() {
        // A huge idle but idle_ttl_secs == 0 disables the condition.
        assert_eq!(
            decide_circuit(0, 0, false, i64::MAX, 0),
            CircuitVerdict::Continue
        );
    }

    // ── precedence: budget beats streak beats stall ────────────────────────

    #[test]
    fn budget_beats_streak_and_stall() {
        // All three conditions hold; budget wins.
        let v = decide_circuit(10, 5, true, 100, 60);
        assert_eq!(v, CircuitVerdict::Trip(CircuitReason::BudgetOverCap));
        assert_eq!(
            match &v {
                CircuitVerdict::Trip(r) => r.as_str(),
                _ => "continue",
            },
            "budget_over_cap"
        );
    }

    #[test]
    fn streak_beats_stall() {
        // Both streak and stall hold (no budget); streak wins.
        let v = decide_circuit(10, 5, false, 100, 60);
        assert_eq!(v, CircuitVerdict::Trip(CircuitReason::FailureStreak));
    }

    // ── determinism ────────────────────────────────────────────────────────

    #[test]
    fn decide_circuit_is_deterministic() {
        let v1 = decide_circuit(10, 5, false, 100, 60);
        let v2 = decide_circuit(10, 5, false, 100, 60);
        assert_eq!(v1, v2);
    }

    // ── every reason slug is stable ────────────────────────────────────────

    #[test]
    fn reason_slugs_are_stable() {
        assert_eq!(CircuitReason::FailureStreak.as_str(), "failure_streak");
        assert_eq!(CircuitReason::BudgetOverCap.as_str(), "budget_over_cap");
        assert_eq!(CircuitReason::Stall.as_str(), "stall");
    }
}
