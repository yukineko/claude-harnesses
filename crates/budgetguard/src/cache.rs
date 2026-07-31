//! Cache-hit health — the verdict side of a number that is measured
//! everywhere and judged nowhere.
//!
//! `gauge` already computes a cache hit rate for its human report
//! (`crates/gauge/src/report.rs`) and prints it as a column. Nothing reads that
//! number back: no threshold, no trend, no verdict. At the fleet's current
//! ~100% hit rate the column looks like a constant, and a metric that is always
//! green and never consulted cannot tell you when it stops being green — the
//! shape CLAUDE.md §3 calls fail-open by silence.
//!
//! Why this does NOT call gauge's `cache_hit_rate`: that function answers "no
//! input at all" with `0.0`. That is a reasonable thing to print in a table and
//! a wrong thing to feed a threshold — a session with no rows yet would read as
//! a total cache collapse. The distinction between "no cache" and "not enough
//! evidence to say" has to survive as a separate value, not as a float. Hence
//! three answers ([`CacheHealth`]), not one number:
//!
//! * [`CacheHealth::NotYetMeasurable`] — below the evidence floor. NOT healthy.
//! * [`CacheHealth::Healthy`] — measured, at or above the threshold.
//! * [`CacheHealth::Degraded`] — measured, below the threshold.
//!
//! There is deliberately no `Default` and no `From<bool>`: a caller must not be
//! able to conjure "healthy" without counters to back it (same reasoning as
//! `harness_core::verdict::Verdict`).

use harness_core::session::Usage;
use std::collections::BTreeMap;

/// Below this hit rate the session is reported as degraded. Chosen from the
/// measured economics rather than taste: input-side spend scales with the MISS
/// fraction, so at 50% the same prefix volume bills at roughly 5.5x what it
/// does at ~100%. A drop that large is a fault, not noise.
pub const DEFAULT_MIN_HIT_RATE: f64 = 0.5;

/// Minimum `input + cache_read` tokens before a rate is judged at all. A fresh
/// session's first turn is all miss by construction (there is nothing cached
/// yet); judging it would fire on every session start and train the operator to
/// ignore the signal.
pub const DEFAULT_MIN_INPUT_TOKENS: u64 = 200_000;

/// Three answers about a session's cache health. Not two, and not a bare float.
#[derive(Debug, Clone, PartialEq)]
#[must_use]
pub enum CacheHealth {
    /// Not enough input volume yet to judge. Distinct from `Healthy` on
    /// purpose: it means "no verdict", not "fine".
    NotYetMeasurable {
        observed: u64,
        floor: u64,
    },
    Healthy {
        rate: f64,
        observed: u64,
    },
    Degraded {
        rate: f64,
        threshold: f64,
        observed: u64,
    },
}

impl CacheHealth {
    /// One operator-readable line. Every variant prints something — a state
    /// that renders as an empty string would be read as "fine", which is the
    /// defect this module exists to remove.
    pub fn describe(&self) -> String {
        match self {
            CacheHealth::NotYetMeasurable { observed, floor } => format!(
                "cache unknown ({} of {} input tokens — below the evidence floor, NOT a pass)",
                observed, floor
            ),
            CacheHealth::Healthy { rate, observed } => {
                format!("cache {:.2}% over {} input tokens", rate * 100.0, observed)
            }
            CacheHealth::Degraded {
                rate,
                threshold,
                observed,
            } => format!(
                "cache hit rate {:.2}% is below the {:.0}% floor over {} input tokens — \
                 input-side spend scales with the MISS fraction, so this session is \
                 re-reading prefix that should have been cached",
                rate * 100.0,
                threshold * 100.0,
                observed
            ),
        }
    }

    pub fn is_degraded(&self) -> bool {
        matches!(self, CacheHealth::Degraded { .. })
    }
}

/// Clamp an operator-supplied threshold into a range that keeps the check
/// ALIVE. A floorless clamp is how a threshold silently becomes "always pass"
/// (CLAUDE.md §3): `0.0` would mean no rate is ever below it, and a NaN
/// comparison is false for every operand, so both are rejected in favour of the
/// default rather than honoured. Returns the effective threshold.
pub fn effective_threshold(configured: f64) -> f64 {
    if !configured.is_finite() || configured <= 0.0 || configured > 1.0 {
        DEFAULT_MIN_HIT_RATE
    } else {
        configured
    }
}

/// Judge a session's cache health from the same per-model counters the cost
/// path already reads.
///
/// `models` empty, or every counter zero, resolves to `NotYetMeasurable` — the
/// restricted side. It must never resolve to `Healthy`: "we saw no tokens" is
/// not evidence that the cache is working.
pub fn assess(
    models: &BTreeMap<String, Usage>,
    configured_threshold: f64,
    floor: u64,
) -> CacheHealth {
    let threshold = effective_threshold(configured_threshold);
    let input: u64 = models.values().map(|u| u.input).sum();
    let cache_read: u64 = models.values().map(|u| u.cache_read).sum();
    let observed = input.saturating_add(cache_read);

    if observed < floor.max(1) {
        return CacheHealth::NotYetMeasurable { observed, floor };
    }
    let rate = cache_read as f64 / observed as f64;
    if rate < threshold {
        CacheHealth::Degraded {
            rate,
            threshold,
            observed,
        }
    } else {
        CacheHealth::Healthy { rate, observed }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn models(pairs: &[(&str, u64, u64)]) -> BTreeMap<String, Usage> {
        let mut m = BTreeMap::new();
        for (name, input, cache_read) in pairs {
            m.insert(
                (*name).to_string(),
                Usage {
                    input: *input,
                    cache_read: *cache_read,
                    ..Usage::default()
                },
            );
        }
        m
    }

    #[test]
    fn a_collapsed_hit_rate_is_reported_as_degraded() {
        // 100k fresh input against 100k cached reads = 50% ... just under a
        // 0.6 floor. Before this module existed the only artefact of this
        // session was a number in a report nobody diffs.
        let h = assess(&models(&[("opus", 100_000, 100_000)]), 0.6, 1_000);
        assert!(h.is_degraded(), "expected Degraded, got {h:?}");
        assert!(
            h.describe().contains("below the"),
            "the degraded line must say what it is below: {}",
            h.describe()
        );
    }

    /// Anti-vacuity control: the check must be capable of answering "fine", or
    /// "it reported degraded" proves nothing.
    #[test]
    fn a_healthy_hit_rate_is_not_reported_as_degraded() {
        let h = assess(&models(&[("opus", 10, 999_990)]), 0.5, 1_000);
        assert!(!h.is_degraded(), "expected Healthy, got {h:?}");
        assert!(matches!(h, CacheHealth::Healthy { .. }));
    }

    #[test]
    fn no_usage_rows_resolve_to_not_yet_measurable_never_to_healthy() {
        let h = assess(&BTreeMap::new(), 0.5, 200_000);
        assert_eq!(
            h,
            CacheHealth::NotYetMeasurable {
                observed: 0,
                floor: 200_000
            },
            "an empty model map must not read as a pass"
        );
        assert!(!h.is_degraded());
        // And it must not silently look like the healthy line either.
        assert!(h.describe().contains("NOT a pass"), "{}", h.describe());
    }

    #[test]
    fn a_session_below_the_evidence_floor_is_not_judged() {
        // All-miss, but only 10k tokens in: a fresh session's first turn.
        let h = assess(&models(&[("opus", 10_000, 0)]), 0.5, 200_000);
        assert!(
            matches!(h, CacheHealth::NotYetMeasurable { .. }),
            "a first turn is all-miss by construction and must not fire: {h:?}"
        );
    }

    #[test]
    fn the_floor_is_crossed_at_the_boundary_not_one_past_it() {
        let at = assess(&models(&[("opus", 200_000, 0)]), 0.5, 200_000);
        assert!(at.is_degraded(), "observed == floor is measurable: {at:?}");
        let below = assess(&models(&[("opus", 199_999, 0)]), 0.5, 200_000);
        assert!(matches!(below, CacheHealth::NotYetMeasurable { .. }));
    }

    #[test]
    fn a_zero_or_nan_threshold_cannot_disable_the_check() {
        // 0.0 would mean "no rate is ever below the floor" and NaN compares
        // false against everything — both are the floorless clamp CLAUDE.md §3
        // names, so both must fall back to the default instead of passing.
        for bad in [0.0_f64, -1.0, f64::NAN, f64::INFINITY, 1.5] {
            assert_eq!(
                effective_threshold(bad),
                DEFAULT_MIN_HIT_RATE,
                "threshold {bad} must not be honoured"
            );
            let h = assess(&models(&[("opus", 100_000, 0)]), bad, 1_000);
            assert!(
                h.is_degraded(),
                "a 0% hit rate must stay degraded under threshold {bad}: {h:?}"
            );
        }
        // A legitimate in-range threshold IS honoured (negative control for the
        // clamp: it must not swallow every configured value).
        assert_eq!(effective_threshold(0.9), 0.9);
    }

    #[test]
    fn counters_are_summed_across_models_not_taken_from_one() {
        // One model perfectly cached, one all-miss and ten times larger. The
        // session verdict must follow the total, not the first entry.
        let h = assess(
            &models(&[("a-good", 0, 100_000), ("b-bad", 1_000_000, 0)]),
            0.5,
            1_000,
        );
        assert!(h.is_degraded(), "{h:?}");
    }
}
