//! Risk-tiered e2e verification — the pure core (no IO, no panics).
//!
//! This is the trajectory-eval sibling of a risk-stratified UI/e2e regression
//! suite. Not every flow deserves the same scrutiny: a config-driven allowlist
//! names the *business-critical* ("core") flows, and everything else is
//! **non-core**.
//!
//! - **Core** flows are verified *every time* by capturing a snapshot and
//!   actually diffing it. Because this repo ships dev tooling with NO deployed
//!   runtime UI, the always-available diff mechanism is a deterministic
//!   **structured-data comparison** (normalized-shape JSON equality). A
//!   perceptual-hash / screenshot comparison is provided behind a trait/enum
//!   boundary so it can be swapped in later — but that path is an honest
//!   **stub**, not implemented. The structured-data path is fully working.
//!
//! - **Non-core** flows get a cheap **existence check** (à la specguard
//!   spec-audit — "is this flow even present?") or, when asked to sample, a
//!   deterministic **seeded low-frequency sampling** decision (no unseeded
//!   randomness).
//!
//! Everything here is a pure function of its inputs so it can be exhaustively
//! unit-tested without touching the filesystem, the clock, or the network.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

// ── config: the core allowlist ─────────────────────────────────────────────────

/// Which comparison strategy a core flow's snapshot is diffed with.
///
/// The `StructuredData` variant is the always-available, deterministic default.
/// `Screenshot` names a perceptual-hash / screenshot boundary that is an honest
/// **stub** — selecting it yields a `DiffOutcome::Stubbed` rather than a real
/// comparison, so callers can wire a real implementation later without changing
/// the tiering logic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffStrategy {
    /// Deterministic normalized-shape structured-data comparison. Fully working.
    #[default]
    StructuredData,
    /// Perceptual-hash / screenshot comparison — a documented, unimplemented stub.
    Screenshot,
    /// Tolerance-based structured-data comparison — see [`fuzzy_diff`]. Unlike
    /// `StructuredData`, small drift (within [`TierConfig::threshold_permille`])
    /// is tolerated as a match instead of any divergence being a hard mismatch.
    FuzzyHash,
}

/// The risk-tiering config: the set of core (business-critical) flow ids, plus
/// the per-config sampling rate applied to non-core flows.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct TierConfig {
    /// The allowlist of business-critical flow ids. Membership is exact-match.
    #[serde(default)]
    pub core: Vec<String>,
    /// How core-flow snapshots are diffed. Defaults to `structured_data`.
    #[serde(default)]
    pub diff_strategy: DiffStrategy,
    /// Non-core sampling rate as `1 in N` runs (0 disables sampling → pure
    /// existence check). Defaults to 0.
    #[serde(default)]
    pub sample_one_in: u64,
    /// Tolerance for [`DiffStrategy::FuzzyHash`], as a distance in permille
    /// (0..=1000; parts per thousand of differing leaves). A drift distance
    /// `<= threshold_permille` is tolerated as a match; strictly greater
    /// escalates to [`DiffOutcome::DriftedBeyondThreshold`]. Defaults to 0
    /// (no tolerance — any drift escalates).
    #[serde(default)]
    pub threshold_permille: u32,
}

/// Which risk tier a flow falls into — a *pure function* of the allowlist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    /// On the core allowlist → verified every run with a real diff.
    Core,
    /// Not on the allowlist → existence check / seeded sampling.
    NonCore,
}

impl TierConfig {
    /// Classify a flow id into its risk [`Tier`]. Pure: same inputs → same tier.
    pub fn tier_of(&self, flow_id: &str) -> Tier {
        if self.core.iter().any(|c| c == flow_id) {
            Tier::Core
        } else {
            Tier::NonCore
        }
    }
}

// ── structured-data diff ────────────────────────────────────────────────────────

/// The outcome of diffing a core flow's captured snapshot against its baseline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum DiffOutcome {
    /// Snapshot matched the baseline under the chosen strategy.
    Match,
    /// Snapshot diverged. `paths` lists the JSON pointer(s) that differ.
    Mismatch { paths: Vec<String> },
    /// The chosen strategy is an unimplemented stub (e.g. `screenshot`).
    Stubbed { strategy: DiffStrategy },
    /// Snapshot diverged from the baseline by more than the configured
    /// [`TierConfig::threshold_permille`] tolerance under [`fuzzy_diff`].
    /// `distance_permille` is the measured drift (differing leaves / total
    /// leaves, in parts per thousand); `paths` lists the differing JSON
    /// pointer(s), sorted. Stored as `u32` (not `f64`) so this type keeps its
    /// `Eq` derive.
    DriftedBeyondThreshold {
        distance_permille: u32,
        paths: Vec<String>,
    },
}

impl DiffOutcome {
    /// True only for a real, passing match. Stubs and mismatches are not a pass.
    ///
    /// Used by [`TierVerdict::verdict_for_diff`] (the tri-state classifier) and
    /// directly by callers that only care about the binary "did the structured
    /// diff match" question without the `NeedsHuman` distinction.
    pub fn is_match(&self) -> bool {
        matches!(self, DiffOutcome::Match)
    }
}

/// Diff a captured snapshot against a baseline using the config's strategy.
///
/// For [`DiffStrategy::StructuredData`] this is a deterministic normalized-shape
/// comparison: object key order is irrelevant (normalized), but values and
/// structure must match. For [`DiffStrategy::Screenshot`] it returns
/// [`DiffOutcome::Stubbed`] — the perceptual-hash path is a documented boundary,
/// not implemented.
pub fn diff_snapshot(
    strategy: DiffStrategy,
    baseline: &serde_json::Value,
    snapshot: &serde_json::Value,
) -> DiffOutcome {
    match strategy {
        DiffStrategy::Screenshot => DiffOutcome::Stubbed { strategy },
        DiffStrategy::StructuredData => {
            let mut paths = Vec::new();
            diff_value("", baseline, snapshot, &mut paths);
            if paths.is_empty() {
                DiffOutcome::Match
            } else {
                paths.sort();
                DiffOutcome::Mismatch { paths }
            }
        }
        // `diff_snapshot` has no threshold parameter (its signature is fixed),
        // so without a configured tolerance this defers to `fuzzy_diff` with
        // zero tolerance — matching `TierConfig::threshold_permille`'s own
        // default of 0. Callers that have a configured threshold should call
        // `fuzzy_diff` directly with it instead of going through this path.
        DiffStrategy::FuzzyHash => fuzzy_diff(baseline, snapshot, 0),
    }
}

/// Recursively collect the JSON-pointer paths at which `baseline` and `snapshot`
/// differ. Object comparison is key-order-independent (a `BTreeMap` normalizes
/// key order), so `{"a":1,"b":2}` and `{"b":2,"a":1}` are equal — that is the
/// "normalized shape" guarantee.
fn diff_value(
    path: &str,
    baseline: &serde_json::Value,
    snapshot: &serde_json::Value,
    out: &mut Vec<String>,
) {
    use serde_json::Value;
    match (baseline, snapshot) {
        (Value::Object(a), Value::Object(b)) => {
            let a: BTreeMap<_, _> = a.iter().collect();
            let b: BTreeMap<_, _> = b.iter().collect();
            // union of keys, deterministically ordered by BTreeMap
            let mut keys: Vec<&String> = a.keys().chain(b.keys()).copied().collect();
            keys.sort();
            keys.dedup();
            for k in keys {
                let child = format!("{}/{}", path, k);
                match (a.get(k), b.get(k)) {
                    (Some(av), Some(bv)) => diff_value(&child, av, bv, out),
                    _ => out.push(child),
                }
            }
        }
        (Value::Array(a), Value::Array(b)) => {
            if a.len() != b.len() {
                out.push(format!("{}/len", path));
            }
            let n = a.len().min(b.len());
            for i in 0..n {
                let child = format!("{}/{}", path, i);
                diff_value(&child, &a[i], &b[i], out);
            }
        }
        (a, b) => {
            if a != b {
                out.push(if path.is_empty() {
                    "/".to_string()
                } else {
                    path.to_string()
                });
            }
        }
    }
}

/// Count the "comparison units" (leaves) a full [`diff_value`] traversal of
/// `(a, b)` would visit — scalars, missing-key/missing-index unions, and array
/// length mismatches each count as one unit — regardless of whether they are
/// equal. Mirrors `diff_value`'s traversal structure exactly, so the number of
/// paths `diff_value` could ever push is always `<=` this count, which keeps
/// [`fuzzy_diff`]'s permille distance well-defined and capped at 1000.
fn count_leaves_pair(a: &serde_json::Value, b: &serde_json::Value) -> usize {
    use serde_json::Value;
    match (a, b) {
        (Value::Object(oa), Value::Object(ob)) => {
            let ma: BTreeMap<_, _> = oa.iter().collect();
            let mb: BTreeMap<_, _> = ob.iter().collect();
            let mut keys: Vec<&String> = ma.keys().chain(mb.keys()).copied().collect();
            keys.sort();
            keys.dedup();
            keys.iter()
                .map(|k| match (ma.get(*k), mb.get(*k)) {
                    (Some(av), Some(bv)) => count_leaves_pair(av, bv),
                    _ => 1,
                })
                .sum()
        }
        (Value::Array(aa), Value::Array(ab)) => {
            let mut total = 0usize;
            if aa.len() != ab.len() {
                total += 1; // mirrors diff_value's single "/len" marker
            }
            let n = aa.len().min(ab.len());
            for i in 0..n {
                total += count_leaves_pair(&aa[i], &ab[i]);
            }
            total
        }
        _ => 1,
    }
}

/// Tolerance-based structured comparison: like [`diff_snapshot`] with
/// [`DiffStrategy::StructuredData`], but small drift is tolerated instead of
/// any divergence being a hard mismatch.
///
/// Computes a deterministic `distance_permille` — `round(1000 * differing /
/// total)` (differing leaf count over total leaf count, rounded half-up as
/// integer arithmetic; 0 when `total` is 0) — and returns
/// [`DiffOutcome::Match`] when `distance_permille <= threshold_permille`,
/// else [`DiffOutcome::DriftedBeyondThreshold`] with the distance and the
/// sorted list of differing JSON-pointer paths. Pure and deterministic: same
/// inputs → same output, no clock, no randomness.
pub fn fuzzy_diff(
    baseline: &serde_json::Value,
    snapshot: &serde_json::Value,
    threshold_permille: u32,
) -> DiffOutcome {
    let mut paths = Vec::new();
    diff_value("", baseline, snapshot, &mut paths);
    paths.sort();

    let total = count_leaves_pair(baseline, snapshot) as u64;
    let differing = paths.len() as u64;
    // Round-half-up integer division; `checked_div` naturally covers the
    // `total == 0` case (distance is 0 when there is nothing to compare).
    let distance_permille: u32 = ((1000 * differing) + total.checked_div(2).unwrap_or(0))
        .checked_div(total)
        .unwrap_or(0) as u32;

    if distance_permille <= threshold_permille {
        DiffOutcome::Match
    } else {
        DiffOutcome::DriftedBeyondThreshold {
            distance_permille,
            paths,
        }
    }
}

// ── non-core: existence check + seeded sampling ─────────────────────────────────

/// The decision for a non-core flow on a given run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum NonCoreDecision {
    /// The flow is not present at all — a hard failure of the existence check.
    Absent,
    /// Present, and this run was not selected for the deeper sample.
    ExistsSkipped,
    /// Present, and this run WAS selected for the deeper sample.
    ExistsSampled,
}

/// Deterministically decide what to do with a non-core flow this run.
///
/// - If `exists` is false → [`NonCoreDecision::Absent`] (existence check failed).
/// - Else if `sample_one_in == 0` → sampling disabled; existence check only →
///   [`NonCoreDecision::ExistsSkipped`].
/// - Else the flow is sampled deterministically `1 in sample_one_in` runs, keyed
///   by a stable hash of `(flow_id, seed, run_index)`. Same inputs → same
///   decision (no unseeded randomness, no clock).
pub fn non_core_decision(
    flow_id: &str,
    exists: bool,
    sample_one_in: u64,
    seed: u64,
    run_index: u64,
) -> NonCoreDecision {
    if !exists {
        return NonCoreDecision::Absent;
    }
    if sample_one_in == 0 {
        return NonCoreDecision::ExistsSkipped;
    }
    if seeded_sample(flow_id, seed, run_index, sample_one_in) {
        NonCoreDecision::ExistsSampled
    } else {
        NonCoreDecision::ExistsSkipped
    }
}

/// Deterministic `1 in n` sampler keyed by `(flow_id, seed, run_index)`.
///
/// Uses a stable FNV-1a hash (not the std `Hasher`, whose output is not
/// guaranteed stable across builds) so the sampling decision is reproducible.
pub fn seeded_sample(flow_id: &str, seed: u64, run_index: u64, n: u64) -> bool {
    if n <= 1 {
        // 1-in-1 (or degenerate 0 handled by caller) → always sample.
        return true;
    }
    let h = fnv1a(flow_id, seed, run_index);
    h % n == 0
}

/// A stable 64-bit FNV-1a hash over `flow_id` mixed with `seed` and `run_index`.
/// Deterministic across builds and platforms.
///
/// Delegates to [`harness_core::hash::fnv1a64`] — the single canonical FNV-1a
/// implementation — instead of re-deriving the algorithm/constants locally.
/// `Fnv1a64::update`-ing the fields in sequence is byte-for-byte equivalent to
/// a one-shot `fnv1a64` over their concatenation (see the harness-core module
/// docs), so this yields the exact same hash values the prior private
/// reimplementation produced.
fn fnv1a(flow_id: &str, seed: u64, run_index: u64) -> u64 {
    let mut h = harness_core::hash::Fnv1a64::new();
    h.update(flow_id.as_bytes());
    h.update(&seed.to_le_bytes());
    h.update(&run_index.to_le_bytes());
    h.finish()
}

// ── top-level verdict ───────────────────────────────────────────────────────────

/// The overall tri-state result of a [`TierVerdict`], for gate exit-code purposes.
///
/// `NeedsHuman` is distinct from `Fail` on purpose: a core flow configured with
/// an unimplemented diff strategy (e.g. `screenshot`) is *not* a real, verified
/// diff failure — it is "this strategy cannot render a verdict yet". Collapsing
/// that into `Fail` would silently gate every run of that flow red forever,
/// masquerading a missing capability as a real regression. `NeedsHuman` gets its
/// own exit code (3) so callers can distinguish "the diff actually mismatched"
/// from "no automated verdict is possible here — a human must look".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// The flow verified cleanly this run.
    Pass,
    /// A real, actionable deviation (mismatch, or existence check failed).
    Fail,
    /// No automated verdict was possible (e.g. an unimplemented diff strategy
    /// stub) — needs a human, not an automatic pass or fail.
    NeedsHuman,
}

impl Verdict {
    /// True only for [`Verdict::Pass`]. Used by callers (the CLI's exit-code
    /// mapping) that just need the binary "is this a clean pass" question
    /// without branching on `Fail` vs `NeedsHuman`.
    pub fn is_pass(&self) -> bool {
        matches!(self, Verdict::Pass)
    }
}

/// The full risk-tiered verdict for one flow on one run — what the CLI reports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TierVerdict {
    pub flow_id: String,
    pub tier: Tier,
    /// Present for core flows: the diff outcome of this run's snapshot.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff: Option<DiffOutcome>,
    /// Present for non-core flows: the existence/sampling decision.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub non_core: Option<NonCoreDecision>,
    /// Overall tri-state result for gate exit-code purposes. Replaces a plain
    /// bool so an unimplemented diff strategy can be reported as `NeedsHuman`
    /// rather than masquerading as a hard `Fail`.
    pub verdict: Verdict,
}

impl TierVerdict {
    /// Derive the tri-state [`Verdict`] for a core flow's [`DiffOutcome`].
    ///
    /// `Stubbed` (an unimplemented diff strategy, e.g. `screenshot`) and
    /// `DriftedBeyondThreshold` (a fuzzy-hash core flow whose drift exceeds its
    /// configured [`TierConfig::threshold_permille`] tolerance) are deliberately
    /// **not** `Fail` — see [`Verdict::NeedsHuman`]'s doc comment. Both are
    /// distinct from an exact `Mismatch`, which is a real, actionable deviation.
    pub fn verdict_for_diff(diff: &DiffOutcome) -> Verdict {
        match diff {
            DiffOutcome::Stubbed { .. } => Verdict::NeedsHuman,
            DiffOutcome::DriftedBeyondThreshold { .. } => Verdict::NeedsHuman,
            _ if diff.is_match() => Verdict::Pass,
            _ => Verdict::Fail,
        }
    }

    /// Derive the tri-state [`Verdict`] for a non-core flow's
    /// [`NonCoreDecision`]. Non-core decisions are always fully automatable
    /// (existence check / seeded sampling), so this never yields `NeedsHuman`.
    pub fn verdict_for_non_core(decision: &NonCoreDecision) -> Verdict {
        match decision {
            NonCoreDecision::Absent => Verdict::Fail,
            NonCoreDecision::ExistsSkipped | NonCoreDecision::ExistsSampled => Verdict::Pass,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn cfg(core: &[&str]) -> TierConfig {
        TierConfig {
            core: core.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    // ── tier branching ──────────────────────────────────────────────────────
    #[test]
    fn tier_core_vs_non_core() {
        let c = cfg(&["checkout", "payment"]);
        assert_eq!(c.tier_of("checkout"), Tier::Core);
        assert_eq!(c.tier_of("payment"), Tier::Core);
        assert_eq!(c.tier_of("settings"), Tier::NonCore);
        assert_eq!(c.tier_of(""), Tier::NonCore);
    }

    #[test]
    fn empty_allowlist_is_all_non_core() {
        let c = cfg(&[]);
        assert_eq!(c.tier_of("anything"), Tier::NonCore);
    }

    // ── structured-data diff: match ──────────────────────────────────────────
    #[test]
    fn structured_match_identical() {
        let a = json!({"a": 1, "b": [1, 2, 3]});
        let b = json!({"a": 1, "b": [1, 2, 3]});
        assert_eq!(
            diff_snapshot(DiffStrategy::StructuredData, &a, &b),
            DiffOutcome::Match
        );
    }

    #[test]
    fn structured_match_key_order_normalized() {
        // Different key order must still match — normalized shape.
        let a = json!({"a": 1, "b": 2});
        let b = json!({"b": 2, "a": 1});
        let out = diff_snapshot(DiffStrategy::StructuredData, &a, &b);
        assert!(out.is_match(), "key order must be normalized: {:?}", out);
    }

    // ── structured-data diff: mismatch ───────────────────────────────────────
    #[test]
    fn structured_mismatch_value() {
        let a = json!({"a": 1});
        let b = json!({"a": 2});
        match diff_snapshot(DiffStrategy::StructuredData, &a, &b) {
            DiffOutcome::Mismatch { paths } => assert_eq!(paths, vec!["/a".to_string()]),
            other => panic!("expected mismatch, got {:?}", other),
        }
    }

    #[test]
    fn structured_mismatch_missing_key_and_len() {
        let a = json!({"a": 1, "b": [1, 2]});
        let b = json!({"a": 1, "b": [1]});
        match diff_snapshot(DiffStrategy::StructuredData, &a, &b) {
            DiffOutcome::Mismatch { paths } => {
                assert!(paths.contains(&"/b/len".to_string()), "paths: {:?}", paths);
            }
            other => panic!("expected mismatch, got {:?}", other),
        }
    }

    #[test]
    fn structured_mismatch_scalar_root() {
        let a = json!(1);
        let b = json!(2);
        match diff_snapshot(DiffStrategy::StructuredData, &a, &b) {
            DiffOutcome::Mismatch { paths } => assert_eq!(paths, vec!["/".to_string()]),
            other => panic!("expected mismatch, got {:?}", other),
        }
    }

    #[test]
    fn structured_diff_is_deterministic_ordering() {
        // The mismatch path list must be sorted (stable) regardless of key order.
        let a = json!({"z": 1, "a": 1});
        let b = json!({"z": 2, "a": 2});
        match diff_snapshot(DiffStrategy::StructuredData, &a, &b) {
            DiffOutcome::Mismatch { paths } => {
                assert_eq!(paths, vec!["/a".to_string(), "/z".to_string()]);
            }
            other => panic!("expected mismatch, got {:?}", other),
        }
    }

    // ── screenshot strategy is an honest stub ────────────────────────────────
    #[test]
    fn screenshot_strategy_is_stubbed() {
        let a = json!({"a": 1});
        let b = json!({"a": 1});
        assert_eq!(
            diff_snapshot(DiffStrategy::Screenshot, &a, &b),
            DiffOutcome::Stubbed {
                strategy: DiffStrategy::Screenshot
            }
        );
    }

    // ── non-core existence + sampling ────────────────────────────────────────
    #[test]
    fn non_core_absent_fails_existence() {
        assert_eq!(
            non_core_decision("f", false, 0, 42, 0),
            NonCoreDecision::Absent
        );
    }

    #[test]
    fn non_core_sampling_disabled_is_existence_only() {
        assert_eq!(
            non_core_decision("f", true, 0, 42, 7),
            NonCoreDecision::ExistsSkipped
        );
    }

    // ── seeded sampling determinism ──────────────────────────────────────────
    #[test]
    fn seeded_sampling_is_reproducible() {
        // Same (flow, seed, run) → same decision, every call.
        let d1 = non_core_decision("checkout", true, 4, 99, 3);
        let d2 = non_core_decision("checkout", true, 4, 99, 3);
        assert_eq!(d1, d2);
        // And the low-level sampler is stable too.
        assert_eq!(
            seeded_sample("checkout", 99, 3, 4),
            seeded_sample("checkout", 99, 3, 4)
        );
    }

    #[test]
    fn seeded_sampling_varies_by_run_index() {
        // Over a window of runs at 1-in-2, both sampled and skipped must occur —
        // proves it actually varies (not a constant), while staying deterministic.
        let sampled = (0..20).filter(|i| seeded_sample("flow", 7, *i, 2)).count();
        assert!(sampled > 0 && sampled < 20, "sampled={} of 20", sampled);
    }

    #[test]
    fn sample_one_in_one_always_samples() {
        assert!(seeded_sample("flow", 0, 0, 1));
        assert!(seeded_sample("flow", 123, 456, 1));
    }

    #[test]
    fn fnv1a_matches_harness_core_canonical_over_concatenated_bytes() {
        // `fnv1a` now delegates to `harness_core::hash::Fnv1a64` instead of a
        // private reimplementation of the algorithm/constants (review-redesign
        // finding 11). Pin that the delegation produces the exact same hash
        // value the old private copy did: streaming `flow_id` bytes then the
        // little-endian `seed`/`run_index` bytes through the canonical hasher
        // must equal a one-shot `fnv1a64` over their concatenation (the
        // harness-core module docs guarantee streaming == one-shot-over-
        // concatenation), so tier-selection behavior is bit-for-bit unchanged.
        let flow_id = "checkout-flow";
        let seed = 1234u64;
        let run_index = 42u64;

        let mut concatenated = Vec::new();
        concatenated.extend_from_slice(flow_id.as_bytes());
        concatenated.extend_from_slice(&seed.to_le_bytes());
        concatenated.extend_from_slice(&run_index.to_le_bytes());
        let expected = harness_core::hash::fnv1a64(&concatenated);

        assert_eq!(fnv1a(flow_id, seed, run_index), expected);
    }

    #[test]
    fn diff_outcome_is_match_semantics() {
        assert!(DiffOutcome::Match.is_match());
        assert!(!DiffOutcome::Mismatch { paths: vec![] }.is_match());
        assert!(!DiffOutcome::Stubbed {
            strategy: DiffStrategy::Screenshot
        }
        .is_match());
    }

    // ── seeded sampling: the RATE is pinned, not just non-constancy ─────────
    //
    // `seeded_sampling_varies_by_run_index` above only proves the sampler isn't
    // a constant. That alone would NOT catch a mutation like changing the
    // `h % n == 0` selector to `h % n < n / 2` (which turns a 1-in-N sampler
    // into a ~50% sampler) — both still "vary". This test runs many run_index
    // values through the seeded sampler and asserts the OBSERVED sampled
    // fraction is close to the intended `1/n`, within a statistical tolerance,
    // so a rate-changing mutation is caught.
    #[test]
    fn seeded_sampling_rate_is_approximately_one_in_n() {
        fn observed_fraction(n: u64, trials: u64) -> f64 {
            let sampled = (0..trials)
                .filter(|i| seeded_sample("checkout-flow", 1234, *i, n))
                .count();
            sampled as f64 / trials as f64
        }

        // n=20 → expect ~5% sampled. A `h % n < n/2` mutation would yield ~50%,
        // which is 10x outside this tolerance band.
        let n = 20u64;
        let trials = 20_000u64;
        let expected = 1.0 / n as f64;
        let observed = observed_fraction(n, trials);
        let tolerance = expected * 0.5; // allow +/-50% relative slack, still << 10x
        assert!(
            (observed - expected).abs() <= tolerance,
            "observed sampled fraction {observed:.4} too far from expected 1/{n}={expected:.4} \
             (tolerance ±{tolerance:.4}) — sampling rate looks wrong, not just non-constant"
        );

        // Same check at a different N to rule out a coincidence at N=20.
        let n2 = 8u64;
        let expected2 = 1.0 / n2 as f64;
        let observed2 = observed_fraction(n2, trials);
        let tolerance2 = expected2 * 0.5;
        assert!(
            (observed2 - expected2).abs() <= tolerance2,
            "observed sampled fraction {observed2:.4} too far from expected 1/{n2}={expected2:.4} \
             (tolerance ±{tolerance2:.4})"
        );
    }

    // ── fuzzy_diff: tolerance-based structured comparison ────────────────────
    #[test]
    fn fuzzy_diff_identical_is_match() {
        let a = json!({"a": 1, "b": 2, "c": 3});
        let b = json!({"a": 1, "b": 2, "c": 3});
        assert_eq!(fuzzy_diff(&a, &b, 0), DiffOutcome::Match);
    }

    #[test]
    fn fuzzy_diff_small_drift_under_threshold_is_match() {
        // 10 leaves, 1 differs → distance 100 permille, well under 500.
        let a = json!({
            "a": 1, "b": 2, "c": 3, "d": 4, "e": 5,
            "f": 6, "g": 7, "h": 8, "i": 9, "j": 10
        });
        let b = json!({
            "a": 1, "b": 2, "c": 3, "d": 4, "e": 5,
            "f": 6, "g": 7, "h": 8, "i": 9, "j": 999
        });
        assert_eq!(fuzzy_diff(&a, &b, 500), DiffOutcome::Match);
    }

    #[test]
    fn fuzzy_diff_drift_over_threshold_reports_distance_and_paths() {
        // 10 leaves, 9 differ → distance 900 permille, well over 100.
        let a = json!({
            "a": 1, "b": 2, "c": 3, "d": 4, "e": 5,
            "f": 6, "g": 7, "h": 8, "i": 9, "j": 10
        });
        let b = json!({
            "a": 1, "b": 20, "c": 30, "d": 40, "e": 50,
            "f": 60, "g": 70, "h": 80, "i": 90, "j": 100
        });
        match fuzzy_diff(&a, &b, 100) {
            DiffOutcome::DriftedBeyondThreshold {
                distance_permille,
                paths,
            } => {
                assert_eq!(distance_permille, 900);
                assert_eq!(paths.len(), 9);
                let mut sorted = paths.clone();
                sorted.sort();
                assert_eq!(paths, sorted, "paths must be sorted");
            }
            other => panic!("expected DriftedBeyondThreshold, got {:?}", other),
        }
    }

    #[test]
    fn fuzzy_diff_is_deterministic() {
        let a = json!({"a": 1, "b": [1, 2, 3]});
        let b = json!({"a": 2, "b": [1, 2, 3]});
        assert_eq!(fuzzy_diff(&a, &b, 200), fuzzy_diff(&a, &b, 200));
    }

    #[test]
    fn fuzzy_diff_boundary_distance_equal_threshold_is_match() {
        // 10 leaves, 1 differs → distance exactly 100 permille.
        let a = json!({
            "a": 1, "b": 2, "c": 3, "d": 4, "e": 5,
            "f": 6, "g": 7, "h": 8, "i": 9, "j": 10
        });
        let b = json!({
            "a": 1, "b": 2, "c": 3, "d": 4, "e": 5,
            "f": 6, "g": 7, "h": 8, "i": 9, "j": 999
        });
        // threshold_permille == distance_permille (100) must be Match, not drift.
        assert_eq!(fuzzy_diff(&a, &b, 100), DiffOutcome::Match);
    }

    #[test]
    fn fuzzy_hash_strategy_serializes_snake_case() {
        let s = serde_json::to_string(&DiffStrategy::FuzzyHash).unwrap();
        assert_eq!(s, "\"fuzzy_hash\"");
    }

    // ── tri-state Verdict derivation ─────────────────────────────────────────
    #[test]
    fn verdict_for_diff_match_is_pass() {
        assert_eq!(
            TierVerdict::verdict_for_diff(&DiffOutcome::Match),
            Verdict::Pass
        );
    }

    #[test]
    fn verdict_for_diff_mismatch_is_fail() {
        assert_eq!(
            TierVerdict::verdict_for_diff(&DiffOutcome::Mismatch {
                paths: vec!["/a".to_string()]
            }),
            Verdict::Fail
        );
    }

    #[test]
    fn verdict_for_diff_stubbed_is_needs_human_not_fail() {
        // The core fix under test: a screenshot-strategy core flow must NOT
        // masquerade as a hard diff failure. It gets its own tri-state verdict.
        let v = TierVerdict::verdict_for_diff(&DiffOutcome::Stubbed {
            strategy: DiffStrategy::Screenshot,
        });
        assert_eq!(v, Verdict::NeedsHuman);
        assert_ne!(v, Verdict::Fail);
        assert!(!v.is_pass());
    }

    #[test]
    fn verdict_for_diff_drifted_beyond_threshold_is_needs_human_not_fail() {
        // A fuzzy-hash core flow whose drift exceeds the configured tolerance
        // must NOT masquerade as a hard diff failure (like an exact Mismatch)
        // and must NOT masquerade as a clean Pass. It gets the same tri-state
        // NeedsHuman treatment as an unimplemented (Stubbed) strategy.
        let v = TierVerdict::verdict_for_diff(&DiffOutcome::DriftedBeyondThreshold {
            distance_permille: 500,
            paths: vec!["/a".to_string()],
        });
        assert_eq!(v, Verdict::NeedsHuman);
        assert_ne!(v, Verdict::Fail);
        assert!(!v.is_pass());
    }

    #[test]
    fn verdict_for_non_core_never_needs_human() {
        assert_eq!(
            TierVerdict::verdict_for_non_core(&NonCoreDecision::Absent),
            Verdict::Fail
        );
        assert_eq!(
            TierVerdict::verdict_for_non_core(&NonCoreDecision::ExistsSkipped),
            Verdict::Pass
        );
        assert_eq!(
            TierVerdict::verdict_for_non_core(&NonCoreDecision::ExistsSampled),
            Verdict::Pass
        );
    }
}
