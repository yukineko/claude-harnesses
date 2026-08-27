//! Multi-sample self-consistency consensus (backlog task 4481bed6).
//!
//! A single generated implementation lets a task-specific hallucination slip
//! past the verifier: the one candidate the verifier saw is the one the worker
//! wrote, so a shared blind spot survives. Self-consistency (fan-out / voting)
//! counters this by generating **N independent candidate implementations** for
//! the *same* task, verifying each, and taking a **majority vote** over the
//! verdicts. Low agreement is itself a signal — the samples disagree, so the
//! task is under-specified or genuinely hard — and triggers an escalation to a
//! stronger model (`opus`) for a tie-break / redo.
//!
//! This module is the deterministic core: given N verdicts it computes the
//! winning candidate, the agreement rate, and the escalate-to-opus decision.
//! The expensive, generative part (spawning N workers, running N verifiers)
//! lives in the `/condukt` SKILL orchestration and is strictly OPT-IN — see
//! [`Plan`] and the cost guard in `config.rs`.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Default agreement threshold: at least half of the N samples must agree on
/// the winning answer, otherwise we escalate. Inclusive lower bound.
pub const DEFAULT_THRESHOLD: f64 = 0.5;

/// Default fan-out width when consensus is enabled. Deliberately small — N-sample
/// generation is N× the cost, so the default keeps the multiplier modest.
pub const DEFAULT_SAMPLES: usize = 3;

/// Documented ceiling on the fan-out width. `Plan` clamps `samples` here so a
/// mis-configured `[consensus] samples = 99` can never fan a single task into a
/// runaway cost. Raise deliberately if ever needed.
pub const MAX_SAMPLES: usize = 5;

/// The stronger model a low-agreement task escalates to. Reuses condukt's model
/// vocabulary (the same top tier the verifier resolver and fugu-router use);
/// this is not a parallel routing scheme.
pub const ESCALATE_TO: &str = "opus";

/// The implicit answer bucket a passing candidate votes for when the caller did
/// not supply a semantic `group`. All plain "it passed verification" candidates
/// then agree with each other (degenerate self-consistency = pass-rate vote).
const IMPLICIT_GROUP: &str = "__pass__";

/// One verifier verdict for a single candidate implementation of a task.
#[derive(Debug, Clone, Deserialize)]
pub struct Verdict {
    /// Which candidate implementation this verdict is for (worktree/branch id).
    pub candidate: String,
    /// The verifier's pass/fail. Only passing candidates cast a vote.
    pub pass: bool,
    /// Optional semantic bucket (a self-consistency "answer signature" the
    /// verifier may assign). When present, passing candidates that share a group
    /// agree with each other; when absent they all fall into [`IMPLICIT_GROUP`].
    #[serde(default)]
    pub group: Option<String>,
}

/// The deterministic consensus decision over N verdicts.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Consensus {
    /// Total number of candidate verdicts (the vote denominator).
    pub n: usize,
    /// How many candidates the verifier passed.
    pub passing: usize,
    /// The selected candidate id (a representative of the winning group), or
    /// `None` when no candidate passed.
    pub winner: Option<String>,
    /// The winning answer bucket, or `None` when no candidate passed.
    pub winning_group: Option<String>,
    /// How many candidates voted for the winning group.
    pub winning_votes: usize,
    /// winning_votes / n — the fraction of ALL samples that agreed on the winner.
    pub agreement_rate: f64,
    /// The threshold actually applied.
    pub threshold: f64,
    /// True when the caller should escalate to `escalate_to` instead of taking
    /// the winner as-is: no candidate passed, a tie for the top group, or the
    /// agreement rate fell below `threshold`.
    pub escalate: bool,
    /// Which model to escalate to (always [`ESCALATE_TO`]; emitted for the skill).
    pub escalate_to: &'static str,
    /// Human-readable explanation of the decision.
    pub reason: String,
    /// `expected - n` (never negative — `expected` is effectively floored at
    /// `n`), the number of verdicts that were supposed to arrive but never
    /// did. A candidate that was spawned and never returned a verdict is a
    /// *silent* loss, indistinguishable at the tally from a candidate that
    /// would have failed/disagreed. `missing > 0` alone forces `escalate`
    /// with `winner = None` (see [`tally`]) regardless of how the received
    /// verdicts agree — counting a verdict that never happened as agreement
    /// is exactly the "could not determine" -> "clean" mapping CLAUDE.md
    /// forbids.
    pub missing: usize,
}

/// Tally N verifier verdicts into a consensus winner + escalation decision.
///
/// `expected` is how many candidate verdicts were actually spawned/supposed
/// to arrive. Pass `verdicts.len()` when the caller has no such count (the
/// pre-existing, backward-compatible behavior: missing-verdict detection is
/// then a no-op, since `expected == n` always). Passing the true spawn count
/// lets a candidate that was spawned but never returned a verdict (a
/// *silent* loss — timeout, crash, dropped worker) be told apart from one
/// that voted; the two are NOT the same thing, even though a naive tally
/// over only the verdicts that arrived cannot distinguish them.
///
/// Voting rules (deterministic, order-independent):
///   * **Verdicts went missing** (`verdicts.len() < expected`) → **escalate**
///     unconditionally, `winner = None`, even if the verdicts that DID arrive
///     agree unanimously. A verdict that never arrived is indistinguishable
///     from one that would have disagreed, so it cannot be silently counted
///     as agreement (CLAUDE.md §3/§6: "could not determine" must resolve to
///     block/escalate, never to clean).
///   * Only PASSING candidates cast a vote; a failing implementation produced no
///     valid answer. Failing candidates still count toward `n` (the denominator),
///     so they lower the agreement rate — a sample that disagreed with the winner.
///   * Each passing candidate votes for its `group` (or [`IMPLICIT_GROUP`]).
///   * The winning group has the most votes; ties are broken lexicographically
///     for a stable *report*, but a tie also forces `escalate = true`.
///   * The winner candidate is the lexicographically smallest candidate id in the
///     winning group (stable representative).
///   * `agreement_rate = winning_votes / n`.
///   * `escalate` iff no candidate passed, OR the top group is tied, OR
///     `agreement_rate < threshold`.
pub fn tally(verdicts: &[Verdict], threshold: f64, expected: usize) -> Consensus {
    let n = verdicts.len();
    // Never negative: a caller-supplied `expected` smaller than what actually
    // arrived (e.g. the default `verdicts.len()`) means nothing is missing.
    let missing = expected.saturating_sub(n);

    // Bucket passing candidates by answer group. BTreeMap keys => deterministic
    // iteration order for stable tie-breaking.
    let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for v in verdicts {
        if v.pass {
            let key = v
                .group
                .clone()
                .unwrap_or_else(|| IMPLICIT_GROUP.to_string());
            groups.entry(key).or_default().push(v.candidate.clone());
        }
    }
    let passing: usize = groups.values().map(|c| c.len()).sum();

    // Fail closed: fewer verdicts arrived than were expected. A silent/missing
    // candidate must not be counted as agreement, so this fires even when the
    // verdicts that DID arrive unanimously agree and would otherwise clear the
    // threshold below. No settled winner is ever returned on a partial vote.
    if missing > 0 {
        return Consensus {
            n,
            passing,
            winner: None,
            winning_group: None,
            winning_votes: 0,
            agreement_rate: 0.0,
            threshold,
            escalate: true,
            escalate_to: ESCALATE_TO,
            reason: format!(
                "{missing} of {expected} expected verdict(s) never arrived ({n} received) — a missing verdict cannot be counted as agreement → escalate"
            ),
            missing,
        };
    }

    // No candidate passed → nothing to select, escalate outright.
    if groups.is_empty() {
        return Consensus {
            n,
            passing: 0,
            winner: None,
            winning_group: None,
            winning_votes: 0,
            agreement_rate: 0.0,
            threshold,
            escalate: true,
            escalate_to: ESCALATE_TO,
            reason: if n == 0 {
                "no verdicts supplied → escalate".to_string()
            } else {
                format!("0/{n} candidates passed verification → escalate to {ESCALATE_TO}")
            },
            missing: 0,
        };
    }

    // Winning group = most votes; first key wins ties (BTreeMap is sorted).
    let max_votes = groups.values().map(|c| c.len()).max().unwrap_or(0);
    let top_groups: Vec<&String> = groups
        .iter()
        .filter(|(_, c)| c.len() == max_votes)
        .map(|(k, _)| k)
        .collect();
    let tie = top_groups.len() > 1;
    let winning_group = top_groups[0].clone();

    // Stable representative: smallest candidate id in the winning group.
    let mut members = groups[&winning_group].clone();
    members.sort();
    let winner = members[0].clone();

    let agreement_rate = if n > 0 {
        max_votes as f64 / n as f64
    } else {
        0.0
    };

    let below = agreement_rate < threshold;
    let escalate = tie || below;

    let pct = agreement_rate * 100.0;
    let thr_pct = threshold * 100.0;
    let reason = if tie {
        format!(
            "tie: {} groups each with {max_votes}/{n} vote(s) → escalate to {ESCALATE_TO} for a tie-break",
            top_groups.len()
        )
    } else if below {
        format!(
            "agreement {max_votes}/{n} = {pct:.0}% < {thr_pct:.0}% threshold → escalate to {ESCALATE_TO}"
        )
    } else {
        format!(
            "consensus: {max_votes}/{n} = {pct:.0}% agreed on '{winning_group}' (>= {thr_pct:.0}% threshold) → take '{winner}'"
        )
    };

    Consensus {
        n,
        passing,
        winner: Some(winner),
        winning_group: Some(winning_group),
        winning_votes: max_votes,
        agreement_rate,
        threshold,
        escalate,
        escalate_to: ESCALATE_TO,
        reason,
        missing: 0,
    }
}

/// The opt-in fan-out plan for a task: whether self-consistency is engaged, and
/// with what width / threshold. Kept separate from [`tally`] so the SKILL can
/// gate the *expensive* generation step deterministically (mirrors the
/// `state autonomy-check` exit-code contract).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Plan {
    /// Whether to fan out at all (global config switch OR a per-task high-risk flag).
    pub enabled: bool,
    /// Number of candidates to generate (clamped to `[1, MAX_SAMPLES]`).
    pub samples: usize,
    /// Threshold to hand to [`tally`] afterwards.
    pub threshold: f64,
    /// Echo of the per-task risk flag that was considered.
    pub risk: Option<String>,
}

/// True iff a per-task risk flag denotes high risk (forces fan-out even when the
/// global switch is off). Case-insensitive; accepts `high`/`critical`.
pub fn risk_is_high(risk: Option<&str>) -> bool {
    matches!(
        risk.map(|r| r.trim().to_ascii_lowercase()).as_deref(),
        Some("high") | Some("critical")
    )
}

/// Compute the fan-out plan. Enabled iff the global switch is on OR the task is
/// flagged high-risk. When enabled, `samples` is clamped to `[2, ceiling]` where
/// `ceiling` is the lower of [`MAX_SAMPLES`] and the per-session concurrency cap
/// ([`harness_core::parallel::session_cap`]) — these candidates are spawned
/// concurrently, so they spend the same session budget as a parallel batch.
/// When disabled the samples field reports 1 (the ordinary single-implementation
/// path).
///
/// PRECEDENCE, stated because it is a real trade-off: the floor of 2 wins over a
/// cap lowered below it. A one-sample "vote" would report an agreement it never
/// measured — a fail-open verdict — so a cap of 1 narrows the panel to 2 rather
/// than degrading it into a rubber stamp. With the default ceiling of 3 this
/// arm is unreachable; it only fires under a manual `HARNESS_MAX_PARALLEL=1`.
pub fn plan(
    global_enabled: bool,
    configured_samples: usize,
    threshold: f64,
    risk: Option<&str>,
) -> Plan {
    let enabled = global_enabled || risk_is_high(risk);
    let ceiling = MAX_SAMPLES
        .min(harness_core::parallel::session_cap())
        .max(2);
    let samples = if enabled {
        configured_samples.clamp(2, ceiling)
    } else {
        1
    };
    Plan {
        enabled,
        samples,
        threshold,
        risk: risk.map(str::to_string),
    }
}

#[cfg(test)]
mod proptests {
    //! Property-based + no-panic floor for [`tally`]: order-independence
    //! (permuting the verdicts never changes the decision), agreement-rate and
    //! vote-count bounds, and totality over arbitrary verdicts + any threshold
    //! (incl. NaN / out-of-range). The consensus decision is control-flow, so
    //! these pin its determinism against generated input, not just examples.
    use super::*;
    use proptest::prelude::*;

    fn any_verdict() -> impl Strategy<Value = Verdict> {
        ("[a-z]{1,4}", any::<bool>(), prop::option::of("[a-z]{1,3}")).prop_map(
            |(candidate, pass, group)| Verdict {
                candidate,
                pass,
                group,
            },
        )
    }

    // The order-independent, decision-relevant projection of a Consensus. Two
    // tallies over permutations of the same multiset must agree on all of these.
    fn key(
        c: &Consensus,
    ) -> (
        Option<String>,
        Option<String>,
        usize,
        usize,
        bool,
        usize,
        u64,
    ) {
        (
            c.winner.clone(),
            c.winning_group.clone(),
            c.winning_votes,
            c.n,
            c.escalate,
            c.passing,
            c.agreement_rate.to_bits(),
        )
    }

    proptest! {
        // Order-independence: reversing or sorting the verdicts (both are
        // permutations of the same multiset) never changes the decision.
        #[test]
        fn tally_is_order_independent(
            verdicts in prop::collection::vec(any_verdict(), 0..12),
            threshold in 0.0f64..=1.0,
        ) {
            let base = tally(&verdicts, threshold, verdicts.len());

            let mut rev = verdicts.clone();
            rev.reverse();
            prop_assert_eq!(key(&base), key(&tally(&rev, threshold, rev.len())), "reverse changed decision");

            let mut sorted = verdicts.clone();
            sorted.sort_by(|a, b| a.candidate.cmp(&b.candidate));
            prop_assert_eq!(key(&base), key(&tally(&sorted, threshold, sorted.len())), "sort changed decision");
        }

        // agreement_rate ∈ [0,1] and winning_votes ≤ n, for any threshold.
        #[test]
        fn tally_bounds_hold(
            verdicts in prop::collection::vec(any_verdict(), 0..12),
            threshold in proptest::num::f64::ANY,
        ) {
            let c = tally(&verdicts, threshold, verdicts.len());
            prop_assert!(
                (0.0..=1.0).contains(&c.agreement_rate),
                "agreement_rate {} out of [0,1]", c.agreement_rate
            );
            prop_assert!(c.winning_votes <= c.n, "winning_votes {} > n {}", c.winning_votes, c.n);
            prop_assert!(c.passing <= c.n);
        }

        // Never panics on any verdicts + any threshold (incl. NaN / ±inf).
        #[test]
        fn tally_never_panics(
            verdicts in prop::collection::vec(any_verdict(), 0..12),
            threshold in proptest::num::f64::ANY,
        ) {
            let c = tally(&verdicts, threshold, verdicts.len());
            // An empty (or all-failing) tally must escalate with no winner.
            if !verdicts.iter().any(|v| v.pass) {
                prop_assert!(c.escalate && c.winner.is_none());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(candidate: &str, pass: bool) -> Verdict {
        Verdict {
            candidate: candidate.to_string(),
            pass,
            group: None,
        }
    }

    fn vg(candidate: &str, pass: bool, group: &str) -> Verdict {
        Verdict {
            candidate: candidate.to_string(),
            pass,
            group: Some(group.to_string()),
        }
    }

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    // ── majority pick ──────────────────────────────────────────────────────

    #[test]
    fn all_pass_unanimous_no_escalation() {
        // 3/3 pass, no groups → all agree on the implicit bucket, agreement 1.0.
        let verdicts = [v("a", true), v("b", true), v("c", true)];
        let c = tally(&verdicts, DEFAULT_THRESHOLD, verdicts.len());
        assert_eq!(c.passing, 3);
        assert_eq!(c.winning_votes, 3);
        assert!(approx(c.agreement_rate, 1.0));
        assert!(!c.escalate, "unanimous pass must not escalate: {c:?}");
        // Winner is the deterministic lexicographically-smallest representative.
        assert_eq!(c.winner.as_deref(), Some("a"));
        assert_eq!(c.winning_group.as_deref(), Some(IMPLICIT_GROUP));
    }

    #[test]
    fn majority_pass_over_one_fail_takes_winner() {
        // 2 pass / 1 fail → 2/3 = 67% >= 50% → consensus, winner = smallest id.
        let verdicts = [v("z", true), v("m", true), v("q", false)];
        let c = tally(&verdicts, DEFAULT_THRESHOLD, verdicts.len());
        assert_eq!(c.passing, 2);
        assert_eq!(c.winning_votes, 2);
        assert!(approx(c.agreement_rate, 2.0 / 3.0));
        assert!(!c.escalate, "67% agreement clears 50%: {c:?}");
        assert_eq!(c.winner.as_deref(), Some("m"), "smallest passing id wins");
    }

    #[test]
    fn grouped_majority_picks_largest_bucket() {
        // Approach A: 3 votes, approach B: 1 vote (all passed verification, but
        // they implemented it two different ways). A wins the self-consistency
        // vote; 3/4 = 75% agreement.
        let verdicts = [
            vg("a1", true, "A"),
            vg("a2", true, "A"),
            vg("a3", true, "A"),
            vg("b1", true, "B"),
        ];
        let c = tally(&verdicts, DEFAULT_THRESHOLD, verdicts.len());
        assert_eq!(c.winning_group.as_deref(), Some("A"));
        assert_eq!(c.winning_votes, 3);
        assert!(approx(c.agreement_rate, 3.0 / 4.0));
        assert!(!c.escalate);
        assert_eq!(c.winner.as_deref(), Some("a1"));
    }

    // ── tie handling ───────────────────────────────────────────────────────

    #[test]
    fn tie_between_groups_escalates() {
        // A: 1, B: 1 — even though both passed, they disagree on the answer with
        // no majority → escalate for a tie-break.
        let verdicts = [vg("b1", true, "B"), vg("a1", true, "A")];
        let c = tally(&verdicts, DEFAULT_THRESHOLD, verdicts.len());
        assert!(c.escalate, "an even split must escalate: {c:?}");
        assert_eq!(c.escalate_to, "opus");
        // Report is still deterministic: smallest group key ('A') is named.
        assert_eq!(c.winning_group.as_deref(), Some("A"));
        assert_eq!(c.winner.as_deref(), Some("a1"));
        assert!(c.reason.contains("tie"), "{}", c.reason);
    }

    #[test]
    fn four_way_even_split_escalates() {
        let verdicts = [
            vg("a", true, "A"),
            vg("b", true, "A"),
            vg("c", true, "B"),
            vg("d", true, "B"),
        ];
        let c = tally(&verdicts, DEFAULT_THRESHOLD, verdicts.len());
        assert!(c.escalate);
        assert_eq!(c.winning_votes, 2);
        assert!(approx(c.agreement_rate, 0.5));
    }

    // ── agreement-rate math + below-threshold → escalate ──────────────────

    #[test]
    fn below_threshold_escalates() {
        // 1 pass / 2 fail → 1/3 = 33% < 50% → escalate.
        let verdicts = [v("a", true), v("b", false), v("c", false)];
        let c = tally(&verdicts, DEFAULT_THRESHOLD, verdicts.len());
        assert_eq!(c.passing, 1);
        assert!(approx(c.agreement_rate, 1.0 / 3.0));
        assert!(c.escalate, "33% agreement is below 50%: {c:?}");
        assert_eq!(c.escalate_to, "opus");
        assert!(c.reason.contains("escalate"));
    }

    #[test]
    fn threshold_is_inclusive_lower_bound() {
        // Exactly at threshold (0.5) must NOT escalate on the agreement criterion.
        let verdicts = [v("a", true), v("b", false)];
        let c = tally(&verdicts, 0.5, verdicts.len());
        assert!(approx(c.agreement_rate, 0.5));
        assert!(
            !c.escalate,
            "agreement == threshold must be accepted: {c:?}"
        );
    }

    #[test]
    fn custom_high_threshold_forces_escalation() {
        // 2/3 = 67% agreement, but a strict 0.9 threshold is not met → escalate.
        let verdicts = [v("a", true), v("b", true), v("c", false)];
        let c = tally(&verdicts, 0.9, verdicts.len());
        assert!(approx(c.agreement_rate, 2.0 / 3.0));
        assert!(c.escalate, "67% < 90% strict threshold: {c:?}");
    }

    // ── all-fail → escalate ────────────────────────────────────────────────

    #[test]
    fn all_fail_escalates_with_no_winner() {
        let verdicts = [v("a", false), v("b", false), v("c", false)];
        let c = tally(&verdicts, DEFAULT_THRESHOLD, verdicts.len());
        assert_eq!(c.passing, 0);
        assert_eq!(c.winner, None);
        assert_eq!(c.winning_group, None);
        assert!(approx(c.agreement_rate, 0.0));
        assert!(c.escalate);
        assert!(c.reason.contains("0/3"), "{}", c.reason);
    }

    #[test]
    fn empty_verdicts_escalate() {
        let c = tally(&[], DEFAULT_THRESHOLD, 0);
        assert_eq!(c.n, 0);
        assert_eq!(c.winner, None);
        assert!(c.escalate);
    }

    // ── N=1 degenerate ─────────────────────────────────────────────────────

    #[test]
    fn n1_pass_is_trivially_consensual() {
        // Degenerate single sample: it "agrees with itself" at 100%. This is the
        // ordinary single-implementation path — never escalates on agreement.
        let c = tally(&[v("only", true)], DEFAULT_THRESHOLD, 1);
        assert_eq!(c.n, 1);
        assert!(approx(c.agreement_rate, 1.0));
        assert!(!c.escalate);
        assert_eq!(c.winner.as_deref(), Some("only"));
    }

    #[test]
    fn n1_fail_escalates() {
        let c = tally(&[v("only", false)], DEFAULT_THRESHOLD, 1);
        assert_eq!(c.passing, 0);
        assert!(c.escalate);
        assert_eq!(c.winner, None);
    }

    // ── determinism: input order does not change the winner ────────────────

    #[test]
    fn winner_is_order_independent() {
        let a = [v("c", true), v("a", true), v("b", true)];
        let b = [v("b", true), v("c", true), v("a", true)];
        assert_eq!(
            tally(&a, 0.5, a.len()).winner,
            tally(&b, 0.5, b.len()).winner
        );
        assert_eq!(tally(&a, 0.5, a.len()).winner.as_deref(), Some("a"));
    }

    // ── opt-in plan gate ───────────────────────────────────────────────────

    #[test]
    fn plan_disabled_by_default_is_single_sample() {
        let p = plan(false, DEFAULT_SAMPLES, DEFAULT_THRESHOLD, None);
        assert!(!p.enabled, "consensus must be opt-in (off by default)");
        assert_eq!(p.samples, 1, "disabled → ordinary single implementation");
    }

    #[test]
    fn plan_global_switch_enables_fanout() {
        let p = plan(true, DEFAULT_SAMPLES, DEFAULT_THRESHOLD, None);
        assert!(p.enabled);
        assert_eq!(p.samples, DEFAULT_SAMPLES);
    }

    #[test]
    fn plan_high_risk_forces_fanout_even_when_global_off() {
        let p = plan(false, DEFAULT_SAMPLES, DEFAULT_THRESHOLD, Some("high"));
        assert!(
            p.enabled,
            "a high-risk task fans out even if the switch is off"
        );
        assert_eq!(p.samples, DEFAULT_SAMPLES);
        assert!(risk_is_high(Some("HIGH")));
        assert!(risk_is_high(Some("critical")));
        assert!(!risk_is_high(Some("low")));
        assert!(!risk_is_high(None));
    }

    #[test]
    fn plan_clamps_samples_to_ceiling_and_floor() {
        // Over the ceiling is clamped down (cost guard); under 2 (while enabled)
        // is floored to 2 so a "vote" always has at least two samples. The
        // effective ceiling is the TIGHTER of the cost guard (`MAX_SAMPLES`) and
        // the per-session concurrency cap — these samples run concurrently.
        let ceiling = MAX_SAMPLES
            .min(harness_core::parallel::session_cap())
            .max(2);
        assert_eq!(plan(true, 99, 0.5, None).samples, ceiling);
        assert_eq!(plan(true, 1, 0.5, None).samples, 2);
    }

    // ── silent vote-loss must fail closed (regression, RED-first) ──────────

    #[test]
    fn missing_verdicts_escalates_even_when_received_verdicts_agree() {
        // 3 candidates expected, only 2 verdicts ever arrive (one verifier
        // went silent), and both received verdicts pass and agree. Without
        // the expected-count check agreement_rate = 2/2 = 100% >= threshold,
        // so the old code would settle a winner — silently treating a
        // verdict that never arrived as one that agreed. That maps "could
        // not determine" to "clean", which this repo forbids (CLAUDE.md
        // §3/§6). The fix must fail closed: no settled winner, escalate,
        // and report the gap via `missing`.
        let verdicts = [v("a", true), v("b", true)];
        let c = tally(&verdicts, DEFAULT_THRESHOLD, 3);
        assert!(
            c.escalate,
            "a silent third verifier must not be counted as agreement: {c:?}"
        );
        assert_eq!(c.winner, None, "must not settle a winner on a partial vote");
        assert_eq!(c.missing, 1, "expected 3 - received 2 = 1 missing verdict");
    }

    #[test]
    fn omitting_expected_preserves_old_behavior() {
        // Non-regression: when the caller does not pass an expected count,
        // `expected` defaults to the number of verdicts actually received
        // (missing-verdict detection is opt-in), so every pre-existing
        // caller behaves exactly as before this change.
        let verdicts = [v("a", true), v("b", true)];
        let c = tally(&verdicts, DEFAULT_THRESHOLD, verdicts.len());
        assert!(!c.escalate);
        assert_eq!(c.winner.as_deref(), Some("a"));
        assert_eq!(c.missing, 0);
    }

    #[test]
    fn fanout_width_is_bounded_by_the_session_cap() {
        // The consensus panel spends the same per-session concurrency budget as
        // an ordinary parallel batch: N candidate implementations are spawned in
        // one message. `MAX_SAMPLES` alone bounded it at 5.
        for requested in [2, 3, 5, 99] {
            let p = plan(true, requested, DEFAULT_THRESHOLD, None);
            assert!(
                p.samples <= harness_core::parallel::SESSION_MAX_PARALLEL,
                "samples {} for requested {requested} exceeds the session cap",
                p.samples
            );
        }
    }
}
