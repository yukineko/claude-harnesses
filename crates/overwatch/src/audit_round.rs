/// Continuous-Audit round metrics ledger (backlog 2630b4c5).
///
/// The Continuous-Audit loop runs periodic adversarial review ROUNDS over a
/// target set (see `docs/review-redesign-implementation-items.md` — 『継続運用の
/// 原則』): each round a finder proposes findings, a verifier CONFIRMS a subset,
/// and confirmed findings are converted into regression tests. The finder/
/// verifier step is inherently LLM-driven, so it is NOT what this module models.
///
/// What IS deterministic and testable — and what this module ships — is the
/// per-round METRICS LEDGER: a small, append-only record of how many new
/// findings a round surfaced, how many were confirmed, and how many regression
/// tests were added. Read back across rounds, it yields the convergence signal
/// the design calls for: the per-round new-findings count should trend DOWN as
/// the target set hardens, and the closure-rate (regression tests added ÷
/// confirmed) shows how diligently confirmed findings are being locked in as
/// tests.
///
/// Everything here is data + pure computation. Emission is fail-soft (see
/// `store::append_audit_round` and `audit_round_cli::record`), matching
/// overwatch's observational / never-break-a-turn invariant.
use serde::{Deserialize, Deserializer, Serialize};

/// Deserialize a round identifier from either a JSON string or a JSON number.
///
/// The `round` field is a `String` today, but legacy `audit_rounds.jsonl`
/// records written when it was a `u64` stored it as a bare JSON number
/// (`{"round":2,...}`). To keep those old ledgers readable, this deserializer
/// accepts both shapes: a string is taken verbatim, and a number is rendered to
/// its decimal string (so a legacy `2` reads back as `"2"`). Any other JSON
/// type is rejected.
fn de_round<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let v = serde_json::Value::deserialize(deserializer)?;
    match v {
        serde_json::Value::String(s) => Ok(s),
        serde_json::Value::Number(n) => Ok(n.to_string()),
        other => Err(serde::de::Error::custom(format!(
            "audit round `round` must be a string or number, got {other}"
        ))),
    }
}

/// A single recorded Continuous-Audit round.
///
/// One record per round per invocation of the audit loop over a target set.
/// `targets` is the (deterministically) normalized list of crates the round
/// reviewed; `new_findings` / `confirmed` / `regression_tests_added` are the
/// round's raw counts.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuditRound {
    /// The round identifier (caller-assigned; an opaque label such as an ISO
    /// week `2026W28`, a date, or a sequence number). It is never used in
    /// arithmetic — convergence relies only on recorded order and display uses
    /// it verbatim — so it is a free-form `String`.
    ///
    /// Backward compatibility: rounds written before this field was a string
    /// were stored as JSON numbers (`{"round":2,...}`). The custom
    /// [`de_round`] deserializer accepts both a JSON string and a JSON number,
    /// reading a legacy numeric `2` as the string `"2"`, so old ledgers keep
    /// reading cleanly.
    #[serde(deserialize_with = "de_round")]
    pub round: String,
    /// The target crates this round reviewed (normalized: trimmed, de-duped,
    /// order-preserving).
    pub targets: Vec<String>,
    /// How many NEW findings the finder surfaced this round (the convergence
    /// signal: this should trend down across rounds).
    pub new_findings: u64,
    /// How many of the round's findings the verifier CONFIRMED.
    pub confirmed: u64,
    /// How many confirmed findings were converted into regression tests this
    /// round.
    pub regression_tests_added: u64,
    /// Unix timestamp when the round was recorded.
    pub ts: i64,
}

impl AuditRound {
    /// Construct a round record. `targets` is normalized (trim, drop blanks,
    /// de-dup preserving first-seen order) so downstream reads are stable.
    pub fn new(
        round: String,
        targets: &[String],
        new_findings: u64,
        confirmed: u64,
        regression_tests_added: u64,
        ts: i64,
    ) -> Self {
        Self {
            round,
            targets: normalize_targets(targets),
            new_findings,
            confirmed,
            regression_tests_added,
            ts,
        }
    }
}

/// Close a Continuous-Audit round: return a copy of the ledger with the
/// `regression_tests_added` of the round identified by `round_id` SET to
/// `tests`, plus whether a matching round was found.
///
/// This is the fix-side feedback the finder-time `record` cannot supply: a round
/// is recorded when the finder/verifier surface findings, at which point no
/// regression tests exist yet (so `regression_tests_added` is necessarily 0).
/// After the confirmed findings are locked in as tests, closing the round feeds
/// that count back so `closure_rate` / `converging` stop lying.
///
/// Semantics:
/// * **SET, not add** — applying the same `tests` twice yields an identical
///   ledger (idempotent backfill, no double count).
/// * **clamped to `confirmed`** — `closure_rate` is documented to live in
///   `[0,1]` (regression_tests_added / confirmed), so a `tests` value greater
///   than the target round's `confirmed` count is clamped down to `confirmed`
///   before being stored. This keeps the invariant true even when the caller
///   (e.g. `overwatch audit-round close --tests`) passes an over-large count.
/// * **most-recent wins** — a round-id is expected to be unique, but the ledger
///   is append-only and cannot forbid duplicates; when several rounds share the
///   id the LAST (most-recently recorded) match is updated and earlier same-id
///   rounds are left untouched (the common "close the round I just ran" case).
/// * **fail-soft** — an unknown `round_id` returns the ledger unchanged with
///   `found == false`; the caller reports it without erroring.
///
/// Pure and side-effect free (the caller persists the result via the store).
pub fn set_round_tests(
    rounds: &[AuditRound],
    round_id: &str,
    tests: u64,
) -> (Vec<AuditRound>, bool) {
    let key = round_id.trim();
    let target = rounds.iter().rposition(|r| r.round.trim() == key);
    let mut out = rounds.to_vec();
    if let Some(i) = target {
        out[i].regression_tests_added = tests.min(out[i].confirmed);
    }
    (out, target.is_some())
}

/// Normalize a target list: trim each entry, drop blanks, de-dup preserving
/// first-seen order. Deterministic and pure.
pub fn normalize_targets(targets: &[String]) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    for t in targets {
        let t = t.trim();
        if t.is_empty() {
            continue;
        }
        if !seen.iter().any(|s| s == t) {
            seen.push(t.to_string());
        }
    }
    seen
}

/// Canonicalize a model identifier for equality comparison: trim + ASCII
/// lowercase. Deliberately light-touch (no tier collapsing) so a caller's
/// exact model string is compared as-given, only normalized for surrounding
/// whitespace and case.
fn canonical_model(m: &str) -> String {
    m.trim().to_ascii_lowercase()
}

/// True iff `finder` and `verifier` denote the SAME model (canonical compare:
/// trim + ASCII-lowercase). This is the deterministic enforcement of the
/// Continuous-Audit `finder != verifier` MUST — the finder and verifier stages
/// must use different models so generation and verification do not share a
/// blind spot (mirrors condukt's `verify::same_model` / `resolve_verifier_model`
/// invariant). Pure and side-effect free.
pub fn same_model(finder: &str, verifier: &str) -> bool {
    canonical_model(finder) == canonical_model(verifier)
}

/// Deterministic finding-id for a finder==verifier model-collision warning,
/// derived from the round id so re-recording the same round yields the SAME id
/// (idempotent key). Pure and side-effect free.
pub fn model_collision_finding_id(round: &str) -> String {
    format!("audit-round-model-collision-{}", round.trim())
}

/// Parse a comma/whitespace-separated `--target` value into a normalized crate
/// list. Accepts both `a,b,c` and `a b c` (and mixtures).
pub fn parse_targets(raw: &str) -> Vec<String> {
    let parts: Vec<String> = raw
        .split([',', ' ', '\t', '\n'])
        .map(|s| s.to_string())
        .collect();
    normalize_targets(&parts)
}

/// Per-round view emitted by [`compute_metrics`], carrying the raw counts plus
/// the round's own closure-rate (regression tests ÷ confirmed for THAT round).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoundMetric {
    /// The round identifier (opaque label, carried through verbatim from the
    /// recorded [`AuditRound`]).
    pub round: String,
    /// New findings surfaced this round (the decreasing-trend signal).
    pub new_findings: u64,
    /// Findings confirmed this round.
    pub confirmed: u64,
    /// Regression tests added this round.
    pub regression_tests_added: u64,
    /// Per-round closure rate = regression_tests_added / confirmed, in `[0,1]`.
    /// `None` when `confirmed == 0` (undefined — nothing to convert).
    pub closure_rate: Option<f64>,
}

/// The convergence report computed across the whole round ledger.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AuditMetrics {
    /// Per-round metrics, in recorded (chronological) order.
    pub rounds: Vec<RoundMetric>,
    /// Total new findings across all rounds.
    pub total_new_findings: u64,
    /// Cumulative confirmed findings across all rounds.
    pub cumulative_confirmed: u64,
    /// Cumulative regression tests added across all rounds.
    pub cumulative_regression_tests_added: u64,
    /// Overall closure rate = cumulative_regression_tests_added /
    /// cumulative_confirmed, in `[0,1]`. `None` when nothing was confirmed.
    pub closure_rate: Option<f64>,
    /// How many trailing rounds the convergence check considered.
    pub convergence_window: usize,
    /// Is the audit CONVERGING? True when new-findings is non-increasing over
    /// the last `convergence_window` rounds (each round ≤ the previous). With
    /// fewer than 2 rounds there is no trend to violate, so it is vacuously
    /// true.
    pub converging: bool,
}

/// The default number of trailing rounds the convergence check considers.
pub const DEFAULT_CONVERGENCE_WINDOW: usize = 3;

/// Compute convergence metrics over the round ledger. Pure and deterministic:
/// no clock, no store access — the caller supplies the rounds already read from
/// the ledger (in recorded order).
///
/// * per-round `closure_rate` = `regression_tests_added / confirmed` (or `None`
///   when `confirmed == 0`);
/// * overall `closure_rate` = cumulative tests ÷ cumulative confirmed;
/// * `converging` = new-findings is NON-INCREASING across the last `window`
///   rounds (i.e. each considered round's new-findings ≤ its predecessor's).
///   Ties (equal counts) count as converging — the trend must not INCREASE.
pub fn compute_metrics(rounds: &[AuditRound], window: usize) -> AuditMetrics {
    let mut per_round: Vec<RoundMetric> = Vec::with_capacity(rounds.len());
    let mut total_new = 0u64;
    let mut cum_confirmed = 0u64;
    let mut cum_tests = 0u64;

    for r in rounds {
        total_new = total_new.saturating_add(r.new_findings);
        cum_confirmed = cum_confirmed.saturating_add(r.confirmed);
        cum_tests = cum_tests.saturating_add(r.regression_tests_added);
        per_round.push(RoundMetric {
            round: r.round.clone(),
            new_findings: r.new_findings,
            confirmed: r.confirmed,
            regression_tests_added: r.regression_tests_added,
            closure_rate: rate(r.regression_tests_added, r.confirmed),
        });
    }

    let converging = is_converging(rounds, window);

    AuditMetrics {
        rounds: per_round,
        total_new_findings: total_new,
        cumulative_confirmed: cum_confirmed,
        cumulative_regression_tests_added: cum_tests,
        closure_rate: rate(cum_tests, cum_confirmed),
        convergence_window: window,
        converging,
    }
}

/// `numer / denom` as a fraction in `[0,1]`, or `None` when `denom == 0`.
fn rate(numer: u64, denom: u64) -> Option<f64> {
    if denom == 0 {
        None
    } else {
        Some(numer as f64 / denom as f64)
    }
}

/// Is new-findings non-increasing over the last `window` rounds? With <2 rounds
/// in scope there is no adjacent pair to violate, so the answer is `true`
/// (vacuously converging). A `window` of 0 is treated as "all rounds".
fn is_converging(rounds: &[AuditRound], window: usize) -> bool {
    if rounds.len() < 2 {
        return true;
    }
    let start = if window == 0 {
        0
    } else {
        rounds.len().saturating_sub(window)
    };
    let scope = &rounds[start..];
    scope
        .windows(2)
        .all(|w| w[1].new_findings <= w[0].new_findings)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round(n: u64, new: u64, confirmed: u64, tests: u64, ts: i64) -> AuditRound {
        AuditRound::new(
            n.to_string(),
            &["specguard".to_string()],
            new,
            confirmed,
            tests,
            ts,
        )
    }

    #[test]
    fn finder_equals_verifier_is_rejected() {
        // Same model (any case / whitespace variant) is a MUST violation.
        assert!(same_model("opus", "opus"));
        assert!(same_model("claude-3-5-sonnet", "claude-3-5-sonnet"));
        assert!(same_model("  Opus  ", "opus"));
        assert!(same_model("CLAUDE-3-5-HAIKU", "claude-3-5-haiku"));
        // Distinct models pass the diversity requirement.
        assert!(!same_model("claude-3-5-sonnet", "claude-3-5-opus"));
        assert!(!same_model("opus", "haiku"));
        assert!(!same_model("sonnet", "haiku"));
    }

    #[test]
    fn model_collision_finding_id_is_derived_from_round() {
        // Idempotent: same round id => same finding id (trimmed).
        assert_eq!(
            model_collision_finding_id("2026W28"),
            "audit-round-model-collision-2026W28"
        );
        assert_eq!(
            model_collision_finding_id("  2026W28  "),
            model_collision_finding_id("2026W28")
        );
    }

    #[test]
    fn normalize_targets_trims_dedups_preserves_order() {
        let got = normalize_targets(&[
            " specguard ".to_string(),
            "blastguard".to_string(),
            "specguard".to_string(),
            "  ".to_string(),
            "stuckguard".to_string(),
        ]);
        assert_eq!(got, vec!["specguard", "blastguard", "stuckguard"]);
    }

    #[test]
    fn parse_targets_accepts_comma_and_space() {
        assert_eq!(
            parse_targets("specguard, blastguard stuckguard"),
            vec!["specguard", "blastguard", "stuckguard"]
        );
        assert_eq!(parse_targets(""), Vec::<String>::new());
    }

    #[test]
    fn audit_round_round_trips_json() {
        let r = round(2, 3, 2, 2, 1000);
        let json = serde_json::to_string(&r).unwrap();
        let back: AuditRound = serde_json::from_str(&json).unwrap();
        assert_eq!(r, back);
    }

    #[test]
    fn round_serializes_as_string() {
        // A string round-id (ISO week) survives a JSON round-trip verbatim.
        let r = AuditRound::new(
            "2026W28".to_string(),
            &["specguard".to_string()],
            1,
            1,
            1,
            1000,
        );
        let json = serde_json::to_string(&r).unwrap();
        assert!(
            json.contains("\"round\":\"2026W28\""),
            "round must serialize as a JSON string: {json}"
        );
        let back: AuditRound = serde_json::from_str(&json).unwrap();
        assert_eq!(back.round, "2026W28");
    }

    #[test]
    fn legacy_numeric_round_deserializes_as_string() {
        // Records written before `round` became a String stored it as a bare
        // JSON number. The backward-compat deserializer must read `{"round":2}`
        // as the string "2" so old audit_rounds.jsonl ledgers stay readable.
        let legacy = r#"{"round":2,"targets":["specguard"],"new_findings":3,"confirmed":2,"regression_tests_added":2,"ts":1000}"#;
        let back: AuditRound = serde_json::from_str(legacy).unwrap();
        assert_eq!(back.round, "2");
        assert_eq!(back.targets, vec!["specguard".to_string()]);
        assert_eq!(back.new_findings, 3);

        // And it flows through compute_metrics carrying the stringified id.
        let m = compute_metrics(&[back], DEFAULT_CONVERGENCE_WINDOW);
        assert_eq!(m.rounds[0].round, "2");
    }

    #[test]
    fn closure_rate_is_tests_over_confirmed() {
        let rounds = vec![round(1, 5, 4, 2, 10), round(2, 2, 2, 2, 20)];
        let m = compute_metrics(&rounds, DEFAULT_CONVERGENCE_WINDOW);
        // Round 1: 2/4 = 0.5 ; Round 2: 2/2 = 1.0
        assert_eq!(m.rounds[0].closure_rate, Some(0.5));
        assert_eq!(m.rounds[1].closure_rate, Some(1.0));
        // Overall: (2+2)/(4+2) = 4/6.
        assert_eq!(m.closure_rate, Some(4.0 / 6.0));
        assert_eq!(m.cumulative_confirmed, 6);
        assert_eq!(m.cumulative_regression_tests_added, 4);
        assert_eq!(m.total_new_findings, 7);
    }

    #[test]
    fn closure_rate_undefined_when_no_confirmed() {
        let rounds = vec![round(1, 3, 0, 0, 10)];
        let m = compute_metrics(&rounds, DEFAULT_CONVERGENCE_WINDOW);
        assert_eq!(m.rounds[0].closure_rate, None);
        assert_eq!(m.closure_rate, None);
    }

    #[test]
    fn converging_when_new_findings_decrease() {
        // 5 -> 3 -> 1 strictly decreasing => converging.
        let rounds = vec![
            round(1, 5, 5, 5, 10),
            round(2, 3, 3, 3, 20),
            round(3, 1, 1, 1, 30),
        ];
        assert!(compute_metrics(&rounds, DEFAULT_CONVERGENCE_WINDOW).converging);
    }

    #[test]
    fn converging_allows_ties() {
        // 3 -> 3 -> 3 non-increasing (flat) => still converging.
        let rounds = vec![
            round(1, 3, 3, 3, 10),
            round(2, 3, 3, 3, 20),
            round(3, 3, 3, 3, 30),
        ];
        assert!(compute_metrics(&rounds, DEFAULT_CONVERGENCE_WINDOW).converging);
    }

    #[test]
    fn not_converging_when_new_findings_increase() {
        // 1 -> 4 within the window => an increase => NOT converging.
        let rounds = vec![round(1, 1, 1, 1, 10), round(2, 4, 4, 4, 20)];
        assert!(!compute_metrics(&rounds, DEFAULT_CONVERGENCE_WINDOW).converging);
    }

    #[test]
    fn window_bounds_the_convergence_check() {
        // Early spike (2 -> 9) then decline (9 -> 3 -> 1). With window=2 only the
        // last two rounds (3 -> 1) are considered => converging, even though the
        // full history had an increase.
        let rounds = vec![
            round(1, 2, 2, 2, 10),
            round(2, 9, 9, 9, 20),
            round(3, 3, 3, 3, 30),
            round(4, 1, 1, 1, 40),
        ];
        assert!(compute_metrics(&rounds, 2).converging);
        // With window=0 (all rounds) the 2->9 increase makes it NOT converging.
        assert!(!compute_metrics(&rounds, 0).converging);
    }

    #[test]
    fn single_round_is_vacuously_converging() {
        let rounds = vec![round(1, 7, 7, 7, 10)];
        assert!(compute_metrics(&rounds, DEFAULT_CONVERGENCE_WINDOW).converging);
    }

    #[test]
    fn empty_ledger_is_converging_with_no_rounds() {
        let m = compute_metrics(&[], DEFAULT_CONVERGENCE_WINDOW);
        assert!(m.rounds.is_empty());
        assert!(m.converging);
        assert_eq!(m.closure_rate, None);
    }

    #[test]
    fn set_round_tests_updates_matching_round_and_metrics_reflect_it() {
        // A round recorded at finding-time carries tests=0 (the fixes don't exist
        // yet). Closing it with the fix-side count must flow into closure_rate.
        let rounds = vec![round(1, 5, 5, 0, 10), round(2, 3, 3, 0, 20)];
        let (out, found) = set_round_tests(&rounds, "2", 3);
        assert!(found, "round id 2 exists");
        assert_eq!(out[0].regression_tests_added, 0, "other rounds untouched");
        assert_eq!(out[1].regression_tests_added, 3, "matching round SET");
        // Observable layer: metrics now report honest closure for that round.
        let m = compute_metrics(&out, DEFAULT_CONVERGENCE_WINDOW);
        assert_eq!(m.rounds[1].closure_rate, Some(1.0)); // 3/3
        assert_eq!(m.cumulative_regression_tests_added, 3);
        assert_eq!(m.closure_rate, Some(3.0 / 8.0)); // (0+3)/(5+3)
    }

    #[test]
    fn set_round_tests_is_idempotent_set_not_add() {
        // SET (not +=) so backfilling the same round twice does NOT double-count.
        let rounds = vec![round(1, 5, 5, 0, 10)];
        let (once, f1) = set_round_tests(&rounds, "1", 5);
        let (twice, f2) = set_round_tests(&once, "1", 5);
        assert!(f1 && f2);
        assert_eq!(once, twice);
        assert_eq!(twice[0].regression_tests_added, 5);
    }

    #[test]
    fn set_round_tests_unknown_id_is_noop_and_reports_not_found() {
        // Fail-soft: an unknown round-id leaves the ledger untouched.
        let rounds = vec![round(1, 5, 5, 0, 10)];
        let (out, found) = set_round_tests(&rounds, "nope", 9);
        assert!(!found);
        assert_eq!(out, rounds);
    }

    #[test]
    fn set_round_tests_clamps_to_confirmed_so_closure_rate_never_exceeds_one() {
        // Closing a round with tests > confirmed (e.g. a miscounted
        // `overwatch audit-round close --tests`) must not push closure_rate
        // above the documented [0,1] range: the stored count is clamped to
        // the round's own `confirmed`.
        let rounds = vec![round(1, 5, 5, 0, 10), round(2, 3, 2, 0, 20)];
        let (out, found) = set_round_tests(&rounds, "2", 9);
        assert!(found, "round id 2 exists");
        assert_eq!(
            out[1].regression_tests_added, 2,
            "tests clamped down to confirmed (2), not stored as 9"
        );
        assert_eq!(out[0].regression_tests_added, 0, "other rounds untouched");

        let m = compute_metrics(&out, DEFAULT_CONVERGENCE_WINDOW);
        assert_eq!(
            m.rounds[1].closure_rate,
            Some(1.0),
            "clamped to exactly 1.0"
        );
        assert!(
            m.rounds[1].closure_rate.unwrap() <= 1.0,
            "closure_rate must never exceed 1.0"
        );
        assert!(
            m.closure_rate.unwrap() <= 1.0,
            "overall closure_rate must never exceed 1.0"
        );
    }

    #[test]
    fn set_round_tests_duplicate_id_closes_most_recent() {
        // Two rounds share id "2026W28" (confirmed 1 then 11). Closing the id
        // targets the MOST RECENTLY recorded (last) match; the earlier is left 0
        // so its closure never exceeds 1.0.
        let a = AuditRound::new("2026W28".to_string(), &["s".to_string()], 1, 1, 0, 10);
        let b = AuditRound::new("2026W28".to_string(), &["s".to_string()], 13, 11, 0, 20);
        let (out, found) = set_round_tests(&[a, b], "2026W28", 11);
        assert!(found);
        assert_eq!(
            out[0].regression_tests_added, 0,
            "earlier same-id untouched"
        );
        assert_eq!(out[1].regression_tests_added, 11, "latest closed");
        let m = compute_metrics(&out, DEFAULT_CONVERGENCE_WINDOW);
        assert_eq!(m.rounds[1].closure_rate, Some(1.0)); // 11/11, not >1.0
    }
}
