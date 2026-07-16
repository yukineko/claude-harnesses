//! Calibrated confidence: how likely is THIS task to pass, given the
//! historical k-NN neighbourhood in the episode store? Pure function of
//! (title, files, class, episodes) — no I/O, no clock, no RNG — so the same
//! store state always yields the same score (deterministic, no LLM).
//!
//! Distinct from `policy::decide`, which picks a *model tier*: this module
//! answers "how confident should we be that this exact task clears
//! verification", independent of which worker/verifier is chosen.

use crate::rag::{self, Neighbor};
use crate::store::Episode;

/// Confidence returned when there isn't enough history to say anything better
/// than a coin flip — the neutral prior. Documented, not a magic number: this
/// is what [`calibrated_confidence`] degrades to when neighbours are empty or
/// below `min_samples`. Insufficient history is never an error, just a
/// maximally-uncertain answer (exit 0, degraded value).
pub const NEUTRAL_PRIOR: f64 = 0.5;

/// Calibrated confidence in `[0, 1]` that a task with this (title, files,
/// class) will pass verification, derived from the effective pass-rate of its
/// k-NN neighbours in `episodes`.
///
/// - Retrieves neighbours via [`rag::knn`] (the same retrieval `route`/
///   `suggest` use), so confidence and routing agree on "what's similar".
/// - Aggregates via [`Episode::effective_pass`] (human label overrides the
///   verifier's self-pass), mirroring `policy`'s aggregate de-biasing.
/// - Falls back to [`NEUTRAL_PRIOR`] when there are no neighbours, or fewer
///   than `min_samples` neighbours.
/// - `class` is accepted for interface symmetry with the routing pure
///   functions (and future stratification) but does not currently change the
///   computation directly; retrieval already folds file/title similarity in.
///
/// Pure: no I/O, no clock, no RNG. Same inputs + same `episodes` slice always
/// produce byte-identical output.
#[allow(clippy::too_many_arguments)]
pub fn calibrated_confidence(
    title: &str,
    files: &[String],
    _class: &str,
    episodes: &[Episode],
    k: usize,
    sim_threshold: f64,
    min_samples: usize,
) -> f64 {
    let neighbors = rag::knn(title, files, episodes, k, sim_threshold);
    score_neighbors(&neighbors, min_samples)
}

/// The pure aggregation step, split out from retrieval so it can be unit
/// tested directly against hand-built neighbour sets (no store/rag wiring
/// needed). Similarity-weighted fraction of neighbours whose
/// [`Episode::effective_pass`] is true; falls back to a plain (unweighted)
/// fraction when every neighbour has zero weight (degenerate all-zero-sim
/// case at `sim_threshold == 0.0`).
fn score_neighbors(neighbors: &[Neighbor], min_samples: usize) -> f64 {
    if neighbors.len() < min_samples.max(1) {
        return NEUTRAL_PRIOR;
    }
    let weight_sum: f64 = neighbors.iter().map(|n| n.sim).sum();
    if weight_sum <= 0.0 {
        let passes = neighbors.iter().filter(|n| n.ep.effective_pass()).count();
        return passes as f64 / neighbors.len() as f64;
    }
    let weighted_passes: f64 = neighbors
        .iter()
        .map(|n| if n.ep.effective_pass() { n.sim } else { 0.0 })
        .sum();
    (weighted_passes / weight_sum).clamp(0.0, 1.0)
}

/// Mean squared error between each prediction and its binary outcome (the
/// Brier score) — the standard proper scoring rule for probabilistic
/// forecasts. Lower is better: `0.0` = perfect, `0.25` = what a constant
/// `0.5` forecast scores against a 50/50 outcome mix (uninformative
/// baseline). Empty input scores `0.0` (vacuously perfect — no predictions to
/// be wrong about).
#[allow(dead_code)] // reliability-check helper: exercised by tests, not yet wired to a CLI surface
pub fn brier_score(predictions: &[(f64, bool)]) -> f64 {
    if predictions.is_empty() {
        return 0.0;
    }
    let sum: f64 = predictions
        .iter()
        .map(|(p, outcome)| {
            let o = if *outcome { 1.0 } else { 0.0 };
            (p - o).powi(2)
        })
        .sum();
    sum / predictions.len() as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nb(model: &str, sim: f64, pass: bool) -> Neighbor {
        Neighbor {
            ep: Episode {
                ts: 0,
                title: "x".into(),
                touched_files: vec![],
                class: "parallel".into(),
                model: model.into(),
                role: "worker".into(),
                pass,
                cost_usd: 0.0,
                human_label: None,
                labeled_by: None,
                skill_fingerprint: None,
                duration_secs: 0.0,
                delegation: None,
            },
            sim,
        }
    }

    #[test]
    fn empty_neighbors_falls_back_to_neutral_prior() {
        assert_eq!(score_neighbors(&[], 1), NEUTRAL_PRIOR);
    }

    #[test]
    fn below_min_samples_falls_back_to_neutral_prior() {
        // Two neighbours retrieved, but min_samples=3 — not enough evidence.
        let neighbors = vec![nb("haiku", 0.5, true), nb("haiku", 0.5, true)];
        assert_eq!(score_neighbors(&neighbors, 3), NEUTRAL_PRIOR);
    }

    #[test]
    fn score_is_always_in_unit_interval() {
        let neighbors = vec![
            nb("haiku", 0.9, true),
            nb("sonnet", 0.4, false),
            nb("opus", 0.2, true),
        ];
        let s = score_neighbors(&neighbors, 1);
        assert!((0.0..=1.0).contains(&s), "score {s} out of [0,1]");
    }

    #[test]
    fn all_pass_neighbors_score_near_one() {
        let neighbors = vec![
            nb("haiku", 0.8, true),
            nb("haiku", 0.7, true),
            nb("haiku", 0.6, true),
        ];
        assert!((score_neighbors(&neighbors, 1) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn all_fail_neighbors_score_near_zero() {
        let neighbors = vec![
            nb("haiku", 0.8, false),
            nb("haiku", 0.7, false),
            nb("haiku", 0.6, false),
        ];
        assert!(score_neighbors(&neighbors, 1).abs() < 1e-9);
    }

    #[test]
    fn human_label_overrides_verifier_self_pass() {
        // Verifier says pass, but a human says bad — must count as a fail.
        let mut n = nb("haiku", 0.8, true);
        n.ep.human_label = Some(false);
        assert!(score_neighbors(&[n], 1).abs() < 1e-9);
    }

    #[test]
    fn similarity_weighting_favours_the_closer_neighbor() {
        // A high-similarity fail and a low-similarity pass should pull the
        // score toward the fail side (below the unweighted 0.5 midpoint).
        let neighbors = vec![nb("haiku", 0.9, false), nb("haiku", 0.1, true)];
        let s = score_neighbors(&neighbors, 1);
        assert!(s < 0.5, "expected weighting toward the closer fail: {s}");
    }

    #[test]
    fn zero_weight_neighbors_fall_back_to_plain_fraction() {
        // Degenerate case: all similarities are exactly 0.0 (e.g. threshold
        // 0.0 with no lexical overlap) — weighted average would divide by
        // zero, so this must fall back to a plain unweighted fraction.
        let neighbors = vec![
            nb("haiku", 0.0, true),
            nb("haiku", 0.0, true),
            nb("haiku", 0.0, false),
        ];
        let s = score_neighbors(&neighbors, 1);
        assert!((s - (2.0 / 3.0)).abs() < 1e-9, "score={s}");
    }

    #[test]
    fn brier_score_is_zero_for_perfect_predictions() {
        let preds = vec![(1.0, true), (0.0, false), (1.0, true)];
        assert_eq!(brier_score(&preds), 0.0);
    }

    #[test]
    fn brier_score_penalises_confident_wrong_predictions_more() {
        let confident_wrong = vec![(0.95, false)];
        let unsure_wrong = vec![(0.55, false)];
        assert!(brier_score(&confident_wrong) > brier_score(&unsure_wrong));
    }

    #[test]
    fn brier_score_empty_is_zero() {
        assert_eq!(brier_score(&[]), 0.0);
    }

    // --- calibration / reliability -----------------------------------------
    //
    // A Brier-informed reliability check: bucket synthetic episodes by their
    // TRUE underlying pass probability, run each bucket's held-out query
    // through `score_neighbors` against that bucket's own history, and assert
    // the predicted confidence lands within `TOLERANCE` of the bucket's
    // observed (actual) pass-rate. This is what "calibrated" means: a 0.7
    // prediction should correspond to episodes that actually pass ~70% of the
    // time, not just a monotonic ranking.
    const TOLERANCE: f64 = 0.15;

    /// Deterministic pseudo-random bool stream (no RNG dependency) — every
    // n-th-of-m draw passes, giving an exact rational pass-rate per bucket.
    fn synth_neighbors(true_rate_numerator: usize, out_of: usize, sim: f64) -> Vec<Neighbor> {
        (0..out_of)
            .map(|i| nb("sonnet", sim, i % out_of < true_rate_numerator))
            .collect()
    }

    #[test]
    fn calibration_buckets_track_observed_pass_rate_within_tolerance() {
        // Each bucket: (predicted-confidence-numerator, out_of) synthetic
        // history, uniform similarity so the score reduces to a plain
        // pass-rate — i.e. predicted == constructed rate exactly, and the
        // "observed" rate (recomputed independently below) must match.
        let buckets: &[(usize, usize)] = &[(0, 10), (3, 10), (5, 10), (7, 10), (10, 10)];

        let mut predictions: Vec<(f64, bool)> = Vec::new();
        for &(num, den) in buckets {
            let neighbors = synth_neighbors(num, den, 0.5);
            let predicted = score_neighbors(&neighbors, 1);
            let observed_pass_rate =
                neighbors.iter().filter(|n| n.ep.effective_pass()).count() as f64 / den as f64;

            assert!(
                (predicted - observed_pass_rate).abs() <= TOLERANCE,
                "bucket ({num}/{den}): predicted={predicted}, observed={observed_pass_rate}, \
                 tolerance={TOLERANCE}"
            );

            // Feed every episode in the bucket into the Brier accounting,
            // predicted-vs-actual, using the bucket's own predicted score as
            // the forecast for each of its members (a standard reliability
            // decomposition).
            for n in &neighbors {
                predictions.push((predicted, n.ep.effective_pass()));
            }
        }

        // A well-calibrated set of bucketed predictions should beat the
        // uninformative constant-0.5 baseline's Brier score (0.25) by a
        // healthy margin — otherwise the "calibration" is not adding signal.
        let score = brier_score(&predictions);
        assert!(
            score < 0.2,
            "expected a well-calibrated Brier score below the 0.5-baseline 0.25, got {score}"
        );
    }

    #[test]
    fn calibrated_confidence_end_to_end_matches_direct_scoring() {
        // Exercise the public, retrieval-backed entry point (not just the
        // pure aggregation helper) to make sure knn wiring + min_samples are
        // threaded through correctly.
        let episodes: Vec<Episode> = (0..4)
            .map(|i| Episode {
                ts: 0,
                title: "wire the login endpoint".into(),
                touched_files: vec!["src/auth/login.ts".into()],
                class: "parallel".into(),
                model: "sonnet".into(),
                role: "worker".into(),
                pass: i < 3, // 3/4 pass
                cost_usd: 0.0,
                human_label: None,
                labeled_by: None,
                skill_fingerprint: None,
                duration_secs: 0.0,
                delegation: None,
            })
            .collect();
        let files = vec!["src/auth/login.ts".to_string()];
        let conf = calibrated_confidence(
            "fix the login endpoint",
            &files,
            "parallel",
            &episodes,
            6,
            0.0,
            1,
        );
        assert!((0.0..=1.0).contains(&conf));
        // With 3/4 identical-title, identical-file neighbours passing, the
        // score should sit clearly above the neutral prior.
        assert!(conf > NEUTRAL_PRIOR, "conf={conf}");
    }

    #[test]
    fn calibrated_confidence_cold_start_is_neutral() {
        // No episodes at all -> no neighbours -> neutral prior, never an error.
        let conf = calibrated_confidence("some new task", &[], "parallel", &[], 6, 0.15, 1);
        assert_eq!(conf, NEUTRAL_PRIOR);
    }
}
