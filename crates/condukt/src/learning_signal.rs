//! Cross-task-learning aggregation: makes the "does injecting a retrieved
//! lesson actually reduce replans?" effect machine-measurable.
//!
//! This is a purely ADDITIVE read-side layer over two ledgers that already
//! exist and are both keyed by `run_id`:
//!   * `harness_core::retrieval` — one [`harness_core::retrieval::RetrievalEvent`]
//!     per run, recording whether a lessons search hit (`hit: bool`).
//!   * `crate::state::load_replan_records` — the per-run replan-decision log,
//!     whose length (or summed `replan_count`, see below) is that run's
//!     replan activity.
//!
//! For each run in the retrieval ledger we look up its total replan count and
//! bucket the run into the "hit" group or the "miss" group. The headline
//! metric, `mean_replan_reduction_ratio`, answers: "how much lower is the
//! mean replan total for runs where a lesson was successfully injected,
//! relative to runs where it wasn't?" A ratio near 1.0 means hits nearly
//! eliminate replanning; 0.0 means no measurable effect; negative means hits
//! replanned *more* (unlikely, but not clamped — the raw signal is more
//! useful than a artificially bounded one).
//!
//! Aggregation is split into a PURE function ([`aggregate`]) that takes
//! already-loaded data, and an I/O wrapper ([`compute`]) that loads both
//! ledgers fail-soft (any error → empty) and calls the pure fn. Tests feed
//! the pure fn synthetic data directly — no real store required.

use crate::config::Config;
use crate::state;
use std::collections::HashMap;
use std::path::Path;

/// Result of aggregating the retrieval ledger against per-run replan totals.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct LearningSignal {
    /// `1 - (mean replan of hit runs / mean replan of miss runs)`.
    /// `None` when the ratio is undefined (see [`aggregate`] doc for the
    /// exact edge cases) — never a `NaN` or `Inf` masquerading as a number.
    pub ratio: Option<f64>,
    /// Mean total replan count across "hit" runs (lesson injected). `None`
    /// only when there are zero hit runs.
    pub numerator_mean: Option<f64>,
    /// Mean total replan count across "miss" runs (no lesson injected).
    /// `None` only when there are zero miss runs.
    pub denominator_mean: Option<f64>,
    /// Number of distinct runs in the hit group.
    pub hit_sample_size: usize,
    /// Number of distinct runs in the miss group.
    pub miss_sample_size: usize,
}

/// Pure aggregation core: takes retrieval events plus a way to resolve a
/// run's total replan count, and produces the [`LearningSignal`]. No I/O, no
/// AI, fully deterministic — every branch is covered by the edge-case tests
/// in this module.
///
/// `replan_total_for_run` is called once per event's `run_id` (events are
/// already one-per-run by the retrieval ledger's own idempotency guarantee;
/// if a caller passes a slice with duplicate `run_id`s, the later event's
/// `hit` wins the bucket assignment for that run — mirrors "last write" but
/// in practice never happens against the real ledger).
pub fn aggregate<F>(
    events: &[harness_core::retrieval::RetrievalEvent],
    mut replan_total_for_run: F,
) -> LearningSignal
where
    F: FnMut(&str) -> usize,
{
    // Bucket runs by hit/miss. A HashMap collapses any duplicate run_ids
    // (last event for a given run_id wins) before we compute means, so the
    // sample sizes always reflect distinct runs, not raw event counts.
    let mut hit_totals: HashMap<&str, usize> = HashMap::new();
    let mut miss_totals: HashMap<&str, usize> = HashMap::new();

    for e in events {
        let total = replan_total_for_run(e.run_id.as_str());
        if e.hit {
            miss_totals.remove(e.run_id.as_str());
            hit_totals.insert(e.run_id.as_str(), total);
        } else {
            hit_totals.remove(e.run_id.as_str());
            miss_totals.insert(e.run_id.as_str(), total);
        }
    }

    let hit_sample_size = hit_totals.len();
    let miss_sample_size = miss_totals.len();

    let numerator_mean = mean(hit_totals.values().copied());
    let denominator_mean = mean(miss_totals.values().copied());

    // Ratio is defined only when both groups are non-empty AND the miss-group
    // mean is a strictly positive divisor (never divide by zero, never
    // propagate a NaN/Inf into the reported metric).
    let ratio = match (numerator_mean, denominator_mean) {
        (Some(num), Some(den)) if den > 0.0 => Some(1.0 - (num / den)),
        _ => None,
    };

    LearningSignal {
        ratio,
        numerator_mean,
        denominator_mean,
        hit_sample_size,
        miss_sample_size,
    }
}

/// Mean of an iterator of `usize` totals, as `f64`. `None` for an empty
/// iterator (never a `0.0/0` NaN).
fn mean(values: impl Iterator<Item = usize> + Clone) -> Option<f64> {
    let count = values.clone().count();
    if count == 0 {
        return None;
    }
    let sum: usize = values.sum();
    Some(sum as f64 / count as f64)
}

/// I/O wrapper: load the retrieval ledger (`harness_core::retrieval::load`)
/// and, for each distinct run_id found there, sum `replan_count` across that
/// run's replan-log records (`state::load_replan_records`), then call
/// [`aggregate`]. Fail-soft throughout: both underlying loaders already
/// degrade to empty on missing/corrupt data, so `compute` never panics and
/// never errors — an absent store simply yields the empty-ledger zero shape.
pub fn compute(cfg: &Config, cwd: &Path) -> LearningSignal {
    let events = harness_core::retrieval::load();
    aggregate(&events, |run_id| {
        state::load_replan_records(cfg, cwd, run_id)
            .iter()
            .map(|r| r.replan_count)
            .sum()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_core::retrieval::RetrievalEvent;
    use std::collections::HashMap as StdHashMap;

    fn ev(run: &str, hit: bool) -> RetrievalEvent {
        RetrievalEvent {
            run_id: run.to_string(),
            query_summary: "q".to_string(),
            hit,
            lesson_ids: Vec::new(),
            k: 3,
            ts: 1000,
        }
    }

    /// Build a `replan_total_for_run` closure backed by a fixed map, for
    /// deterministic synthetic-data tests. Missing run_id → 0 (mirrors
    /// `load_replan_records` returning an empty vec / sum 0).
    fn totals_fn(map: StdHashMap<&'static str, usize>) -> impl FnMut(&str) -> usize {
        move |run_id: &str| map.get(run_id).copied().unwrap_or(0)
    }

    const EPS: f64 = 1e-9;

    #[test]
    fn known_distribution_expected_ratio() {
        // hit group: r1=1, r2=1 → mean 1.0
        // miss group: r3=3, r4=3, r5=3 → mean 3.0
        // ratio = 1 - (1.0/3.0) = 0.6666666...
        let events = vec![
            ev("r1", true),
            ev("r2", true),
            ev("r3", false),
            ev("r4", false),
            ev("r5", false),
        ];
        let totals: StdHashMap<&str, usize> =
            [("r1", 1), ("r2", 1), ("r3", 3), ("r4", 3), ("r5", 3)].into();
        let sig = aggregate(&events, totals_fn(totals));

        assert_eq!(sig.hit_sample_size, 2);
        assert_eq!(sig.miss_sample_size, 3);
        assert!((sig.numerator_mean.unwrap() - 1.0).abs() < EPS);
        assert!((sig.denominator_mean.unwrap() - 3.0).abs() < EPS);
        let ratio = sig.ratio.expect("ratio must be defined");
        assert!(
            (ratio - (2.0 / 3.0)).abs() < 1e-6,
            "expected ~0.6667, got {ratio}"
        );
    }

    #[test]
    fn empty_ledger_yields_none_and_zero_sizes() {
        let sig = aggregate(&[], totals_fn(StdHashMap::new()));
        assert_eq!(sig.ratio, None);
        assert_eq!(sig.numerator_mean, None);
        assert_eq!(sig.denominator_mean, None);
        assert_eq!(sig.hit_sample_size, 0);
        assert_eq!(sig.miss_sample_size, 0);
    }

    #[test]
    fn hit_only_yields_none_ratio_but_computed_numerator() {
        let events = vec![ev("r1", true), ev("r2", true)];
        let totals: StdHashMap<&str, usize> = [("r1", 2), ("r2", 4)].into();
        let sig = aggregate(&events, totals_fn(totals));

        assert_eq!(sig.hit_sample_size, 2);
        assert_eq!(sig.miss_sample_size, 0);
        assert!((sig.numerator_mean.unwrap() - 3.0).abs() < EPS);
        assert_eq!(sig.denominator_mean, None);
        assert_eq!(sig.ratio, None, "no miss group → ratio undefined");
    }

    #[test]
    fn miss_only_yields_none_ratio_but_computed_denominator() {
        let events = vec![ev("r1", false), ev("r2", false)];
        let totals: StdHashMap<&str, usize> = [("r1", 1), ("r2", 5)].into();
        let sig = aggregate(&events, totals_fn(totals));

        assert_eq!(sig.hit_sample_size, 0);
        assert_eq!(sig.miss_sample_size, 2);
        assert_eq!(sig.numerator_mean, None);
        assert!((sig.denominator_mean.unwrap() - 3.0).abs() < EPS);
        assert_eq!(sig.ratio, None, "no hit group → ratio undefined");
    }

    #[test]
    fn single_run_each_group() {
        let events = vec![ev("r1", true), ev("r2", false)];
        let totals: StdHashMap<&str, usize> = [("r1", 0), ("r2", 2)].into();
        let sig = aggregate(&events, totals_fn(totals));

        assert_eq!(sig.hit_sample_size, 1);
        assert_eq!(sig.miss_sample_size, 1);
        assert_eq!(sig.numerator_mean, Some(0.0));
        assert_eq!(sig.denominator_mean, Some(2.0));
        // ratio = 1 - (0/2) = 1.0
        assert!((sig.ratio.unwrap() - 1.0).abs() < EPS);
    }

    #[test]
    fn mean_miss_zero_yields_none_and_never_panics() {
        // Both miss-group runs have zero replans → denominator_mean == 0.0,
        // which must guard the division rather than divide by zero.
        let events = vec![ev("r1", true), ev("r2", false), ev("r3", false)];
        let totals: StdHashMap<&str, usize> = [("r1", 5), ("r2", 0), ("r3", 0)].into();
        let sig = aggregate(&events, totals_fn(totals));

        assert_eq!(sig.hit_sample_size, 1);
        assert_eq!(sig.miss_sample_size, 2);
        assert_eq!(sig.numerator_mean, Some(5.0));
        assert_eq!(sig.denominator_mean, Some(0.0));
        assert_eq!(
            sig.ratio, None,
            "mean_miss == 0.0 must guard the division, not divide by zero"
        );
    }

    #[test]
    fn duplicate_run_id_last_event_wins_bucket() {
        // If the same run_id appears twice with a flipped hit flag (should
        // never happen against the real idempotent ledger, but the pure fn
        // must not panic or double-count), the later event wins the bucket.
        let events = vec![ev("r1", true), ev("r1", false)];
        let totals: StdHashMap<&str, usize> = [("r1", 4)].into();
        let sig = aggregate(&events, totals_fn(totals));

        assert_eq!(sig.hit_sample_size, 0);
        assert_eq!(sig.miss_sample_size, 1);
        assert_eq!(sig.numerator_mean, None);
        assert_eq!(sig.denominator_mean, Some(4.0));
    }

    #[test]
    fn missing_run_in_totals_fn_defaults_to_zero_replans() {
        // Mirrors load_replan_records returning an empty vec (sum 0) for a
        // run that never triggered a replan decision.
        let events = vec![ev("r1", true), ev("r2", false)];
        let sig = aggregate(&events, totals_fn(StdHashMap::new()));

        assert_eq!(sig.numerator_mean, Some(0.0));
        assert_eq!(sig.denominator_mean, Some(0.0));
        assert_eq!(sig.ratio, None, "mean_miss == 0.0 → ratio undefined");
    }

    #[test]
    fn serializes_null_for_none_fields() {
        let sig = aggregate(&[], totals_fn(StdHashMap::new()));
        let j = serde_json::to_string(&sig).unwrap();
        assert!(j.contains("\"ratio\":null"));
        assert!(j.contains("\"numerator_mean\":null"));
        assert!(j.contains("\"denominator_mean\":null"));
        assert!(j.contains("\"hit_sample_size\":0"));
        assert!(j.contains("\"miss_sample_size\":0"));
    }
}
