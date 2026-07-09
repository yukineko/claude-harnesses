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
}

impl DiffOutcome {
    /// True only for a real, passing match. Stubs and mismatches are not a pass.
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
fn fnv1a(flow_id: &str, seed: u64, run_index: u64) -> u64 {
    const OFFSET: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x00000100000001b3;
    let mut h = OFFSET;
    for b in flow_id.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(PRIME);
    }
    for b in seed.to_le_bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(PRIME);
    }
    for b in run_index.to_le_bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(PRIME);
    }
    h
}

// ── top-level verdict ───────────────────────────────────────────────────────────

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
    /// Overall pass/fail for gate exit-code purposes.
    pub pass: bool,
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
    fn diff_outcome_is_match_semantics() {
        assert!(DiffOutcome::Match.is_match());
        assert!(!DiffOutcome::Mismatch { paths: vec![] }.is_match());
        assert!(!DiffOutcome::Stubbed {
            strategy: DiffStrategy::Screenshot
        }
        .is_match());
    }
}
