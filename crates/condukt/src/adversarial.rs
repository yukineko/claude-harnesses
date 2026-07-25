//! Adversarial refutation panel — verification-side fan-out.
//!
//! Prototype for the gap called out in `docs/loop-engineering-evaluation.ja.md`:
//! this repo's verify layer (propguard / reviewgate / specguard / the single
//! condukt verifier) is essentially *single-pass self-verification or one
//! subprocess checker*. There is no "adversarial verify" where **several
//! independent skeptics each try to refute the same completion and vote**.
//!
//! [`crate::consensus`] already fans out **generation**: N independent candidate
//! implementations of the *same task*, verify each, majority-vote the winning
//! candidate, escalate-to-opus on low agreement. That counters a *generation*
//! blind spot (the one candidate the verifier saw is the one the worker wrote).
//!
//! This module fans out **verification** instead, and is deliberately
//! *complementary* to consensus (not a replacement): N independent skeptics read
//! the **same single completed artifact** and each tries to *refute* the "it's
//! done / it's correct" claim. It counters a *verification* blind spot — a lone
//! verifier that shares the worker's blind spot rubber-stamps a
//! wrong-but-plausible change. Because this is meant for **high-stakes
//! completions only** (a change touching a GATE crate — the fleet defense gates
//! whose own regressions are the most expensive), it is biased toward refutation
//! and **fails closed**: the artifact passes only if the panel is big enough AND
//! no majority refutes AND (optionally) there is no lone dissent worth a human's
//! eye.
//!
//! This file is the **deterministic core only**: given N ballots it computes a
//! block / escalate / pass decision. The generative, non-deterministic part —
//! spawning N independent skeptic subagents, ideally on *different* models so
//! they do not share a blind spot — lives in the `/condukt` SKILL orchestration
//! and is strictly **OPT-IN** (see [`plan`]). Same split as consensus: the
//! decision is control-flow and lives here in code; the discovery lives in the
//! skill.

use serde::{Deserialize, Serialize};

/// Default panel width when the panel is engaged. Small on purpose — each
/// skeptic is a full independent review, so the cost multiplier is real.
pub const DEFAULT_PANEL: usize = 3;

/// Hard ceiling on the panel width so a mis-set `size = 99` cannot fan a single
/// completion into a runaway review cost. Mirrors `consensus::MAX_SAMPLES`.
pub const MAX_PANEL: usize = 5;

/// Default refute ratio at/above which the panel blocks: a simple majority.
/// Inclusive, so an even split (tie) also blocks — fail-closed on a gate.
pub const DEFAULT_BLOCK_RATIO: f64 = 0.5;

/// Minimum number of *effective* (non-abstaining) skeptics required to clear a
/// high-stakes completion. Below this we cannot claim to have adversarially
/// verified anything, so we fail closed. A panel of one is not adversarial.
pub const DEFAULT_MIN_VOTERS: usize = 2;

/// Float slop for the ratio comparison so an exact majority (e.g. 2/4 = 0.5)
/// lands on the inclusive-block side despite binary rounding. Mirrors
/// `mutategate::KILL_RATE_EPSILON`.
const RATIO_EPSILON: f64 = 1e-9;

/// The fleet **GATE crates** whose changes make a completion "high-stakes" and
/// thus worth an adversarial panel.
///
/// This used to be a second hand-copied literal array in this file — it and
/// `crates/tdd/src/config.rs`'s copy both silently drifted and lost `overwatch`
/// at different points. It is now a re-export of the single canonical
/// definition in [`harness_core::fleet::GATE_CRATES`], so there is exactly one
/// place the crate-name *value* can be edited; this file and `tdd`'s copy can
/// no longer diverge from each other by construction (the Rust compiler, not a
/// cross-source script, is what keeps a `pub use` re-export identical to what
/// it re-exports).
///
/// Enforced against the remaining non-Rust sources (shell/Python/Markdown) by
/// `scripts/check-gate-crates-sync.py`, which now parses
/// `crates/harness-core/src/fleet.rs` as the sole tracked Rust source (see
/// that script's module docstring).
pub use harness_core::fleet::GATE_CRATES;

/// One skeptic's ballot on the single artifact under review.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Ballot {
    /// The skeptic found a concrete, grounded defect — the completion should not
    /// pass. (The prose grounding travels in [`Vote::reason`].)
    Refute,
    /// The skeptic actively tried to refute and could not: the artifact survived.
    Pass,
    /// The skeptic could not reach a judgment (out of scope / not enough
    /// context). Casts no vote either way and lowers the *effective* panel size.
    Abstain,
}

/// One skeptic's verdict on the single artifact under review.
#[derive(Debug, Clone, Deserialize)]
pub struct Vote {
    /// Which independent skeptic cast this ballot (subagent/model id). Recorded
    /// for the audit trail; not used in the tally.
    pub skeptic: String,
    /// The refute / pass / abstain ballot.
    pub ballot: Ballot,
    /// Optional grounding for a `Refute` (file:line, failing property, ...).
    #[serde(default)]
    pub reason: Option<String>,
}

/// The policy the panel is adjudicated under. Separated from [`adjudicate`] so a
/// caller can tighten the gate (raise `min_voters`, lower `block_ratio`) for the
/// very highest-stakes changes without editing the core.
#[derive(Debug, Clone, Copy)]
pub struct Policy {
    /// Effective (non-abstain) ballots required to even consider passing; below
    /// this the panel fails closed (`block`).
    pub min_voters: usize,
    /// `refutes / effective` at/above which the panel blocks. Inclusive.
    pub block_ratio: f64,
    /// When true, any refute *below* the block ratio (a lone/minority dissent on
    /// a high-stakes gate) yields `escalate` rather than `pass` — route it to a
    /// human or a stronger reviewer instead of silently accepting.
    pub escalate_on_dissent: bool,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            min_voters: DEFAULT_MIN_VOTERS,
            block_ratio: DEFAULT_BLOCK_RATIO,
            escalate_on_dissent: true,
        }
    }
}

/// The deterministic adjudication over N skeptic ballots.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Panel {
    /// Total ballots supplied (the raw denominator).
    pub n: usize,
    /// How many skeptics refuted.
    pub refutes: usize,
    /// How many skeptics tried and could not refute.
    pub passes: usize,
    /// How many skeptics abstained (no vote either way).
    pub abstains: usize,
    /// `n - abstains` — the vote denominator that actually counts.
    pub effective: usize,
    /// `refutes / effective` (0.0 when `effective == 0`).
    pub refute_ratio: f64,
    /// The gate verdict: true = do not let the completion through.
    pub block: bool,
    /// True when the panel neither cleanly passed nor blocked and a human /
    /// stronger reviewer should decide (minority dissent).
    pub escalate: bool,
    /// One of `"block" | "escalate" | "pass"` — the three mutually-exclusive
    /// outcomes (redundant with `block`/`escalate` but convenient for the skill).
    pub outcome: &'static str,
    /// The `min_voters` actually applied.
    pub min_voters: usize,
    /// The `block_ratio` actually applied.
    pub block_ratio: f64,
    /// Human-readable explanation of the decision.
    pub reason: String,
    /// The grounded objections from every refuting skeptic, as
    /// `"<skeptic>: <reason>"` (or just `"<skeptic>"` when no reason was given),
    /// in input order. Empty when nobody refuted. Surfaces *why* the panel
    /// blocked/escalated so the human/skill sees the concrete objections.
    pub refutations: Vec<String>,
    /// `expected - n` (never negative — `expected` is clamped up to `n` first),
    /// the number of ballots that were supposed to arrive but never did. A
    /// skeptic that was spawned and never returned a ballot is a *silent* vote
    /// loss, indistinguishable at the tally from a skeptic that would have
    /// refuted. `missing > 0` alone is enough to force `escalate` (see
    /// [`adjudicate`] rule 2.5) regardless of what the received ballots say —
    /// counting a vote that never happened as a non-objection is exactly the
    /// "could not determine" -> "clean" mapping CLAUDE.md forbids.
    pub missing: usize,
}

/// Adjudicate N skeptic ballots into a fail-closed gate decision.
///
/// `expected` is how many skeptics were actually spawned/supposed to vote.
/// Pass `votes.len()` when the caller has no such count (the pre-existing,
/// backward-compatible behavior: missing-vote detection is then a no-op,
/// since `expected == n` always). Passing the true spawn count lets a
/// skeptic that was spawned but never returned a ballot (a *silent* vote
/// loss — timeout, crash, dropped subagent) be told apart from a skeptic
/// that voted `Pass`; the two are NOT the same thing, even though a naive
/// tally over only the ballots that arrived cannot distinguish them.
///
/// Deterministic and order-independent. The rules, in order:
///   1. **Too few effective skeptics** (`effective < min_voters`) → **block**.
///      A high-stakes completion that could not be adversarially verified does
///      not get the benefit of the doubt.
///   2. **Majority (or configured ratio) refute** (`refutes/effective >=
///      block_ratio`, inclusive so a tie blocks) → **block**.
///   3. **Votes went missing** (`votes.len() < expected`) → **escalate**,
///      unconditionally, even if the received ballots unanimously pass and
///      clear `min_voters`. A ballot that never arrived is indistinguishable
///      from one that would have refuted, so it cannot be silently counted as
///      a non-objection (CLAUDE.md §3/§6: "could not determine" must resolve
///      to block/escalate, never to clean).
///   4. **Minority dissent** (some refutes, below the block ratio) →
///      **escalate** when `escalate_on_dissent`, else fall through to pass.
///   5. Otherwise (no refutes, enough voters, no missing votes) → **pass**.
pub fn adjudicate(votes: &[Vote], policy: &Policy, expected: usize) -> Panel {
    let n = votes.len();
    // Never negative: a caller-supplied `expected` smaller than what actually
    // arrived (e.g. the default `votes.len()`) means nothing is missing.
    let missing = expected.saturating_sub(n);
    let refutes = votes.iter().filter(|v| v.ballot == Ballot::Refute).count();
    let passes = votes.iter().filter(|v| v.ballot == Ballot::Pass).count();
    let abstains = votes.iter().filter(|v| v.ballot == Ballot::Abstain).count();
    let effective = refutes + passes;
    let refute_ratio = if effective > 0 {
        refutes as f64 / effective as f64
    } else {
        0.0
    };

    // Carry each refuter's grounding into the decision so a block/escalate shows
    // *why* (the concrete objections), not just a count.
    let refutations: Vec<String> = votes
        .iter()
        .filter(|v| v.ballot == Ballot::Refute)
        .map(|v| match &v.reason {
            Some(r) if !r.trim().is_empty() => format!("{}: {}", v.skeptic, r),
            _ => v.skeptic.clone(),
        })
        .collect();

    let mk = |block: bool, escalate: bool, outcome: &'static str, reason: String| Panel {
        n,
        refutes,
        passes,
        abstains,
        effective,
        refute_ratio,
        block,
        escalate,
        outcome,
        min_voters: policy.min_voters,
        block_ratio: policy.block_ratio,
        reason,
        refutations: refutations.clone(),
        missing,
    };

    // Rule 1 — fail closed: not enough independent skeptics actually voted.
    if effective < policy.min_voters {
        return mk(
            true,
            false,
            "block",
            format!(
                "only {effective} effective skeptic(s) (< min {}) — cannot adversarially verify a high-stakes completion → fail-closed block",
                policy.min_voters
            ),
        );
    }

    // Rule 2 — majority (or configured ratio) refute. Inclusive lower bound so a
    // tie (e.g. 2 refute / 2 pass) blocks.
    let blocks_on_ratio = refutes as f64 + RATIO_EPSILON >= policy.block_ratio * effective as f64;
    if blocks_on_ratio {
        let pct = refute_ratio * 100.0;
        let thr_pct = policy.block_ratio * 100.0;
        return mk(
            true,
            false,
            "block",
            format!(
                "{refutes}/{effective} skeptics refuted ({pct:.0}% >= {thr_pct:.0}% block ratio) → block"
            ),
        );
    }

    // Rule 3 — fail closed: fewer ballots arrived than were expected. A
    // silent/missing skeptic must not be counted as a non-objection, so this
    // fires even when the ballots that DID arrive are unanimous passes that
    // would otherwise clear straight to "pass" at rule 5.
    if missing > 0 {
        return mk(
            false,
            true,
            "escalate",
            format!(
                "{missing} of {expected} expected skeptic(s) never voted ({n} ballot(s) received) — a missing vote cannot be counted as a non-objection → escalate"
            ),
        );
    }

    // Rule 4 — minority dissent: below the block ratio but not unanimous.
    if refutes > 0 && policy.escalate_on_dissent {
        return mk(
            false,
            true,
            "escalate",
            format!(
                "{refutes}/{effective} skeptics refuted (below the {:.0}% block ratio) — minority dissent on a high-stakes gate → escalate to a human / stronger reviewer",
                policy.block_ratio * 100.0
            ),
        );
    }

    // Rule 5 — unanimous survive (or dissent-escalation disabled with a
    // sub-majority of refutes, which the ratio check already cleared).
    mk(
        false,
        false,
        "pass",
        format!("{passes}/{effective} skeptics could not refute ({refutes} refute(s)) → pass"),
    )
}

/// True iff any changed path lives under a GATE crate (`crates/<gate>/...`),
/// which makes a completion high-stakes and worth an adversarial panel.
pub fn touches_gate_crate(files: &[String]) -> bool {
    files.iter().any(|f| {
        let f = f.replace('\\', "/");
        GATE_CRATES
            .iter()
            .any(|g| f.contains(&format!("crates/{g}/")))
    })
}

/// The opt-in fan-out plan for a completion: whether to engage a panel, how wide,
/// and under what policy knobs. Kept separate from [`adjudicate`] so the SKILL
/// can gate the *expensive* N-skeptic step deterministically (mirrors
/// `consensus::plan` and the `state autonomy-check` exit-code contract).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PanelPlan {
    /// Whether to convene a panel at all (global switch OR high-stakes change).
    pub engaged: bool,
    /// How many independent skeptics to spawn (clamped to `[2, MAX_PANEL]` when
    /// engaged; 1 — i.e. the ordinary single-verifier path — when not).
    pub size: usize,
    /// Effective-voter floor handed to [`adjudicate`] afterward.
    pub min_voters: usize,
    /// Block ratio handed to [`adjudicate`] afterward.
    pub block_ratio: f64,
    /// Echo of whether the change touched a GATE crate (forced engagement).
    pub high_stakes: bool,
}

/// Compute the panel plan. Engaged iff the global switch is on OR the change is
/// high-stakes (touched a GATE crate). `size` is clamped to `[2, MAX_PANEL]` when
/// engaged (a one-skeptic "panel" is not adversarial); reports 1 when not.
pub fn plan(
    global_enabled: bool,
    configured_size: usize,
    policy: &Policy,
    high_stakes: bool,
) -> PanelPlan {
    let engaged = global_enabled || high_stakes;
    let size = if engaged {
        configured_size.clamp(2, MAX_PANEL)
    } else {
        1
    };
    PanelPlan {
        engaged,
        size,
        min_voters: policy.min_voters,
        block_ratio: policy.block_ratio,
        high_stakes,
    }
}

#[cfg(test)]
mod proptests {
    //! Property floor for [`adjudicate`]: it is control-flow, so pin its
    //! determinism against generated input — order-independence, exhaustive &
    //! mutually-exclusive outcomes, count conservation, and totality (never
    //! panics on any ballots or any policy).
    use super::*;
    use proptest::prelude::*;

    fn any_ballot() -> impl Strategy<Value = Ballot> {
        prop_oneof![
            Just(Ballot::Refute),
            Just(Ballot::Pass),
            Just(Ballot::Abstain),
        ]
    }

    fn any_vote() -> impl Strategy<Value = Vote> {
        (any_ballot(), any::<u8>()).prop_map(|(ballot, id)| Vote {
            skeptic: format!("s{id}"),
            ballot,
            reason: None,
        })
    }

    fn any_policy() -> impl Strategy<Value = Policy> {
        (0usize..6, -1.0f64..2.0, any::<bool>()).prop_map(|(mv, br, esc)| Policy {
            min_voters: mv,
            block_ratio: br,
            escalate_on_dissent: esc,
        })
    }

    proptest! {
        #[test]
        fn adjudicate_is_order_independent(mut votes in prop::collection::vec(any_vote(), 0..12), pol in any_policy()) {
            let a = adjudicate(&votes, &pol, votes.len());
            votes.reverse();
            let b = adjudicate(&votes, &pol, votes.len());
            // Counts and the decision are invariant to input order.
            prop_assert_eq!(a.block, b.block);
            prop_assert_eq!(a.escalate, b.escalate);
            prop_assert_eq!(a.outcome, b.outcome);
            prop_assert_eq!(a.refutes, b.refutes);
        }

        #[test]
        fn outcome_is_exhaustive_and_exclusive(votes in prop::collection::vec(any_vote(), 0..12), pol in any_policy()) {
            let p = adjudicate(&votes, &pol, votes.len());
            // Exactly one of the three outcomes holds, and it matches the flags.
            prop_assert!(matches!(p.outcome, "block" | "escalate" | "pass"));
            prop_assert_eq!(p.block, p.outcome == "block");
            prop_assert_eq!(p.escalate, p.outcome == "escalate");
            prop_assert!(!(p.block && p.escalate));
        }

        #[test]
        fn counts_are_conserved(votes in prop::collection::vec(any_vote(), 0..12), pol in any_policy()) {
            let p = adjudicate(&votes, &pol, votes.len());
            prop_assert_eq!(p.refutes + p.passes + p.abstains, p.n);
            prop_assert_eq!(p.effective, p.refutes + p.passes);
        }

        #[test]
        fn never_panics(votes in prop::collection::vec(any_vote(), 0..30), pol in any_policy()) {
            let _ = adjudicate(&votes, &pol, votes.len());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(skeptic: &str, ballot: Ballot) -> Vote {
        Vote {
            skeptic: skeptic.into(),
            ballot,
            reason: None,
        }
    }

    #[test]
    fn unanimous_pass_passes() {
        let votes = vec![
            v("a", Ballot::Pass),
            v("b", Ballot::Pass),
            v("c", Ballot::Pass),
        ];
        let p = adjudicate(&votes, &Policy::default(), votes.len());
        assert_eq!(p.outcome, "pass");
        assert!(!p.block && !p.escalate);
        assert_eq!(p.effective, 3);
    }

    #[test]
    fn majority_refute_blocks() {
        let votes = vec![
            v("a", Ballot::Refute),
            v("b", Ballot::Refute),
            v("c", Ballot::Pass),
        ];
        let p = adjudicate(&votes, &Policy::default(), votes.len());
        assert_eq!(p.outcome, "block");
        assert!(p.block);
    }

    #[test]
    fn refutations_carry_skeptic_and_reason() {
        let votes = vec![
            Vote {
                skeptic: "s1".into(),
                ballot: Ballot::Refute,
                reason: Some("detect.rs:477 fail-open".into()),
            },
            Vote {
                skeptic: "s2".into(),
                ballot: Ballot::Refute,
                reason: None,
            },
            v("s3", Ballot::Pass),
        ];
        let p = adjudicate(&votes, &Policy::default(), votes.len());
        assert_eq!(p.outcome, "block");
        assert_eq!(
            p.refutations,
            vec!["s1: detect.rs:477 fail-open".to_string(), "s2".to_string()]
        );
    }

    #[test]
    fn no_refutes_yields_empty_refutations() {
        let votes = vec![
            v("a", Ballot::Pass),
            v("b", Ballot::Pass),
            v("c", Ballot::Pass),
        ];
        let p = adjudicate(&votes, &Policy::default(), votes.len());
        assert!(p.refutations.is_empty());
    }

    #[test]
    fn tie_blocks_fail_closed() {
        // 2 refute / 2 pass = exactly 0.5 → inclusive block.
        let votes = vec![
            v("a", Ballot::Refute),
            v("b", Ballot::Refute),
            v("c", Ballot::Pass),
            v("d", Ballot::Pass),
        ];
        let p = adjudicate(&votes, &Policy::default(), votes.len());
        assert_eq!(p.outcome, "block", "an even split must fail closed");
    }

    #[test]
    fn below_min_voters_blocks() {
        // One lone skeptic passing is not an adversarial panel → block.
        let votes = vec![v("a", Ballot::Pass)];
        let p = adjudicate(&votes, &Policy::default(), votes.len());
        assert_eq!(p.outcome, "block");
        assert!(p.block);
    }

    #[test]
    fn all_abstain_blocks() {
        let votes = vec![
            v("a", Ballot::Abstain),
            v("b", Ballot::Abstain),
            v("c", Ballot::Abstain),
        ];
        let p = adjudicate(&votes, &Policy::default(), votes.len());
        assert_eq!(p.effective, 0);
        assert_eq!(p.outcome, "block", "no effective voters → fail-closed");
    }

    #[test]
    fn minority_dissent_escalates() {
        // 1 refute / 3 pass = 0.25 < 0.5 → escalate (a lone skeptic dissented).
        let votes = vec![
            v("a", Ballot::Refute),
            v("b", Ballot::Pass),
            v("c", Ballot::Pass),
            v("d", Ballot::Pass),
        ];
        let p = adjudicate(&votes, &Policy::default(), votes.len());
        assert_eq!(p.outcome, "escalate");
        assert!(p.escalate && !p.block);
    }

    #[test]
    fn minority_dissent_passes_when_escalation_disabled() {
        let votes = vec![
            v("a", Ballot::Refute),
            v("b", Ballot::Pass),
            v("c", Ballot::Pass),
            v("d", Ballot::Pass),
        ];
        let policy = Policy {
            escalate_on_dissent: false,
            ..Policy::default()
        };
        let p = adjudicate(&votes, &policy, votes.len());
        assert_eq!(p.outcome, "pass");
    }

    #[test]
    fn abstains_lower_the_effective_denominator() {
        // 1 refute, 1 pass, 2 abstain → effective 2, ratio 0.5 → block (tie).
        let votes = vec![
            v("a", Ballot::Refute),
            v("b", Ballot::Pass),
            v("c", Ballot::Abstain),
            v("d", Ballot::Abstain),
        ];
        let p = adjudicate(&votes, &Policy::default(), votes.len());
        assert_eq!(p.effective, 2);
        assert_eq!(p.abstains, 2);
        assert_eq!(p.outcome, "block");
    }

    #[test]
    fn stricter_block_ratio_blocks_on_single_refute() {
        // block_ratio 0.25 → 1/4 refute meets the bar → block, not escalate.
        let votes = vec![
            v("a", Ballot::Refute),
            v("b", Ballot::Pass),
            v("c", Ballot::Pass),
            v("d", Ballot::Pass),
        ];
        let policy = Policy {
            block_ratio: 0.25,
            ..Policy::default()
        };
        let p = adjudicate(&votes, &policy, votes.len());
        assert_eq!(p.outcome, "block");
    }

    #[test]
    fn touches_gate_crate_detects_gates() {
        assert!(touches_gate_crate(&[
            "crates/blastguard/src/detect.rs".into(),
            "README.md".into(),
        ]));
        assert!(touches_gate_crate(&["crates/mutategate/src/lib.rs".into()]));
        // `overwatch` is a GATE crate too: it computes the canary health-gate
        // decision the other gates' rollouts depend on. It had silently gone
        // missing from GATE_CRATES once, exempting the Continuous-Audit crate
        // from the very panel that loop relies on — assert it here so the crate
        // owning the semantics catches a revert, not just the Python checker.
        assert!(touches_gate_crate(&["crates/overwatch/src/lib.rs".into()]));
        assert!(touches_gate_crate(&[
            "crates/overwatch/skills/continuous-audit/SKILL.md".into(),
            "README.md".into(),
        ]));
        assert!(!touches_gate_crate(&["docs/specs/overwatch.md".into()]));
        // A non-gate crate is not high-stakes.
        assert!(!touches_gate_crate(&[
            "crates/condukt/src/main.rs".into(),
            "docs/foo.md".into(),
        ]));
        // Substring safety: a path merely mentioning a gate name elsewhere does
        // not count — it must be the `crates/<gate>/` segment.
        assert!(!touches_gate_crate(&["docs/specs/blastguard.md".into()]));
        assert!(!touches_gate_crate(&[]));
    }

    #[test]
    fn plan_disabled_by_default_is_single_verifier() {
        let plan = plan(false, DEFAULT_PANEL, &Policy::default(), false);
        assert!(!plan.engaged);
        assert_eq!(plan.size, 1);
    }

    #[test]
    fn plan_high_stakes_forces_panel_even_when_global_off() {
        let plan = plan(false, DEFAULT_PANEL, &Policy::default(), true);
        assert!(plan.engaged);
        assert_eq!(plan.size, DEFAULT_PANEL);
        assert!(plan.high_stakes);
    }

    #[test]
    fn plan_global_switch_engages_panel() {
        let plan = plan(true, DEFAULT_PANEL, &Policy::default(), false);
        assert!(plan.engaged);
        assert!(plan.size >= 2);
    }

    #[test]
    fn plan_clamps_size_to_ceiling_and_floor() {
        assert_eq!(plan(true, 99, &Policy::default(), false).size, MAX_PANEL);
        // A configured 1 is floored to 2 when engaged (1 is not a panel).
        assert_eq!(plan(true, 1, &Policy::default(), false).size, 2);
    }

    // ── silent vote-loss must fail closed (regression, RED-first) ──────────

    #[test]
    fn missing_votes_escalates_even_when_received_votes_all_pass() {
        // 3 skeptics expected, only 2 ballots ever arrive (one went silent),
        // and BOTH received ballots pass. Without the expected-count check
        // this clears min_voters (2 >= 2) and refutes == 0, so the old code
        // returned "pass" — silently treating a verifier that never ran as a
        // verifier that did not object. That maps "could not determine" to
        // "clean", which this repo forbids (CLAUDE.md §3/§6). The fix must
        // fail closed to `escalate` and report the gap via `missing`.
        let votes = vec![v("a", Ballot::Pass), v("b", Ballot::Pass)];
        let p = adjudicate(&votes, &Policy::default(), 3);
        assert_eq!(
            p.outcome, "escalate",
            "a silent third skeptic must not be counted as a non-objection: {p:?}"
        );
        assert!(!p.block, "missing votes escalate, they do not block: {p:?}");
        assert_eq!(p.missing, 1, "expected 3 - received 2 = 1 missing vote");
    }

    #[test]
    fn omitting_expected_preserves_old_behavior() {
        // Non-regression: when the caller does not know (or does not pass) an
        // expected count, `expected` defaults to the number of votes actually
        // received (missing-vote detection is opt-in), so every pre-existing
        // caller behaves exactly as before this change.
        let votes = vec![v("a", Ballot::Pass), v("b", Ballot::Pass)];
        let p = adjudicate(&votes, &Policy::default(), votes.len());
        assert_eq!(p.outcome, "pass");
        assert_eq!(p.missing, 0);
    }
}
