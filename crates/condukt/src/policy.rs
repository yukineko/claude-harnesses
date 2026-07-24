//! `condukt policy decide` — the central graded autonomy policy engine.
//!
//! Graduates condukt's autonomy from one flat `cfg.autonomous` bool to a
//! per-decision policy: a decision's `risk` × `reversibility` × `confidence`
//! deterministically maps to `Auto` (proceed unattended), `Escalate` (ask the
//! human — the one surviving 質疑 channel) or `Block` (hard stop; never even
//! ask). Judgment — what risk/reversibility a concrete decision carries — stays
//! LLM-side; this module owns only the deterministic mapping, so it is a pure,
//! fully unit-testable core (mirrors `oracle.rs` / `editgate.rs`). No panics.

use std::fmt;

/// A three-valued level for each policy dimension. `Low < Medium < High` as a
/// magnitude (higher risk = more dangerous, higher reversibility = easier to
/// undo, higher confidence = surer it is correct).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Low,
    Medium,
    High,
}

impl Level {
    /// Numeric magnitude used by the scoring rule (Low=0, Medium=1, High=2).
    fn rank(self) -> i32 {
        match self {
            Level::Low => 0,
            Level::Medium => 1,
            Level::High => 2,
        }
    }

    /// Bucket a continuous calibrated confidence score in `[0, 1]` into the
    /// three-valued confidence [`Level`]. PURE: no I/O, total, never panics.
    ///
    /// This is the adapter that lets a *calibrated* confidence signal (e.g.
    /// fugu-router's "history says this class of task passes" score) feed the
    /// same `confidence` axis that `decide` consumes. Semantics are aligned
    /// with `decide`'s score rule (`risk − reversibility − confidence`, where a
    /// higher confidence *rank* pushes toward `Auto`): a HIGH calibrated score
    /// (near 1.0 — "history says this passes") maps to [`Level::High`] so it
    /// lowers the decision score toward `Auto`, and a LOW score (near 0.0) maps
    /// to [`Level::Low`].
    ///
    /// Documented thresholds bucketing `[0, 1]`:
    /// - `score < 0.34` → [`Level::Low`]
    /// - `0.34 ≤ score < 0.67` → [`Level::Medium`]
    /// - `score ≥ 0.67` → [`Level::High`]
    ///
    /// Out-of-range inputs are clamped into `[0, 1]` first (so `-5.0` → `Low`,
    /// `2.0` → `High`). A non-finite score (`NaN`) is treated as the least
    /// confident value, [`Level::Low`], rather than panicking.
    pub fn from_score(score: f64) -> Level {
        // NaN is unordered under comparisons; treat it as the safe (least
        // confident) bucket so the mapping stays total and never surprises.
        if score.is_nan() {
            return Level::Low;
        }
        let s = score.clamp(0.0, 1.0);
        if s < 0.34 {
            Level::Low
        } else if s < 0.67 {
            Level::Medium
        } else {
            Level::High
        }
    }
}

/// Parse a level from a case-insensitive token. Accepts `low`, `medium`/`med`,
/// `high`. Returns `None` for anything else — callers surface that as an input
/// error rather than panicking.
pub fn parse_level(raw: &str) -> Option<Level> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "low" | "l" => Some(Level::Low),
        "medium" | "med" | "m" => Some(Level::Medium),
        "high" | "h" => Some(Level::High),
        _ => None,
    }
}

/// The policy verdict. Ordered by restrictiveness: `Auto` (least) < `Escalate`
/// < `Block` (most).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Auto,
    Escalate,
    Block,
}

impl Decision {
    /// Restrictiveness rank (Auto=0, Escalate=1, Block=2). Used to state and
    /// test the monotonicity invariant.
    #[cfg(test)]
    fn restrictiveness(self) -> i32 {
        match self {
            Decision::Auto => 0,
            Decision::Escalate => 1,
            Decision::Block => 2,
        }
    }
}

impl fmt::Display for Decision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Decision::Auto => "auto",
            Decision::Escalate => "escalate",
            Decision::Block => "block",
        };
        f.write_str(s)
    }
}

/// Map a decision's `risk`, `reversibility` and `confidence` to a [`Decision`].
///
/// Total over all 27 inputs. Guarantees (each pinned by a unit test):
/// - **Monotonicity**: raising `risk`, lowering `reversibility`, or lowering
///   `confidence` never yields a *less* restrictive decision.
/// - **Hard stop**: a high-risk *and* irreversible action is always `Block`,
///   regardless of confidence — you cannot be confident enough to auto-run an
///   irreversible catastrophe.
/// - Otherwise a `risk − reversibility − confidence` score thresholds into
///   `Auto` (comfortably safe), `Escalate` (ambiguous middle) or `Block`.
pub fn decide(risk: Level, reversibility: Level, confidence: Level) -> Decision {
    // Hard stop: high risk AND irreversible is never automatable, and asking a
    // human to approve an irreversible catastrophe is not a real choice either.
    if risk == Level::High && reversibility == Level::Low {
        return Decision::Block;
    }

    let score = risk.rank() - reversibility.rank() - confidence.rank();
    if score <= -2 {
        Decision::Auto
    } else if score >= 1 {
        Decision::Block
    } else {
        // score ∈ {-1, 0}: the ambiguous middle asks the human.
        Decision::Escalate
    }
}

/// Policy posture for resolving a merge conflict / mid-flight runtime overlap
/// (design 625aa170 decision B). A conflict resolution may ONLY `Escalate` or
/// `Block` — it MUST NOT `Auto`. An automatic pick-a-side IS last-writer-wins,
/// the exact failure this feature kills, so the policy half never chooses a
/// side unattended: it clamps any `Auto` verdict up to `Escalate` (ask the
/// human) while leaving `Escalate`/`Block` untouched. Pure; total; no panics.
///
/// (There is deliberately NO opt-in that lets this return `Auto` — the DEFAULT
/// and only posture is Escalate/Block.)
pub fn decide_conflict_resolution(
    risk: Level,
    reversibility: Level,
    confidence: Level,
) -> Decision {
    match decide(risk, reversibility, confidence) {
        // A conflict is never auto-resolvable: the safest a "just proceed"
        // verdict can be is to ASK, never to silently pick a side.
        Decision::Auto => Decision::Escalate,
        other => other,
    }
}

/// Policy posture for a decision that CANNOT be meaningfully tested (CLAUDE.md
/// §2). When a change cannot be observed by a test — the observation is
/// impossible, or a test would assert nothing — the surviving 質疑 channel is to
/// ask the human: §2's "untestable → ask a human" gate must NEVER be closed by a
/// self-answer. So an untestable decision may ONLY `Escalate` or `Block`, never
/// `Auto`: it clamps any `Auto` verdict up to `Escalate` (ask the human) while
/// leaving `Escalate`/`Block` untouched. The clamp only ever RAISES
/// restrictiveness — it never relaxes an already-gated verdict. Pure; total; no
/// panics.
///
/// (Exactly like [`decide_conflict_resolution`], there is deliberately NO opt-in
/// that lets this return `Auto` — the DEFAULT and only posture is
/// Escalate/Block. An auto-self-answered "this was untestable so I decided for
/// you" is the precise failure this clamp kills.)
pub fn decide_untestable(risk: Level, reversibility: Level, confidence: Level) -> Decision {
    match decide(risk, reversibility, confidence) {
        // An untestable decision is never auto-answerable: the safest a "just
        // proceed" verdict can be is to ASK a human, never to silently self-answer.
        Decision::Auto => Decision::Escalate,
        other => other,
    }
}

#[cfg(test)]
mod proptests {
    //! Property-based floor for [`decide`]: monotonicity and the irreversible
    //! hard-stop, checked over the full generated Level space rather than the
    //! curated example rows. `decide` is total over 27 inputs so these are
    //! effectively exhaustive, but expressing them as properties documents the
    //! contract and guards it against future edits to the scoring rule.
    use super::*;
    use proptest::prelude::*;

    fn any_level() -> impl Strategy<Value = Level> {
        prop_oneof![Just(Level::Low), Just(Level::Medium), Just(Level::High)]
    }

    // Restrictiveness order: Auto < Escalate < Block.
    fn restrictiveness(d: Decision) -> i32 {
        match d {
            Decision::Auto => 0,
            Decision::Escalate => 1,
            Decision::Block => 2,
        }
    }

    fn rank(l: Level) -> i32 {
        match l {
            Level::Low => 0,
            Level::Medium => 1,
            Level::High => 2,
        }
    }

    proptest! {
        // Raising risk never makes the decision LESS restrictive (others fixed).
        #[test]
        fn decide_monotone_nondecreasing_in_risk(
            r1 in any_level(), r2 in any_level(), rev in any_level(), conf in any_level(),
        ) {
            let (lo, hi) = if rank(r1) <= rank(r2) { (r1, r2) } else { (r2, r1) };
            prop_assert!(
                restrictiveness(decide(lo, rev, conf)) <= restrictiveness(decide(hi, rev, conf)),
                "risk monotonicity broken"
            );
        }

        // LOWERING reversibility never makes the decision less restrictive: a
        // harder-to-undo action is at least as gated (others fixed).
        #[test]
        fn decide_monotone_in_reversibility(
            risk in any_level(), v1 in any_level(), v2 in any_level(), conf in any_level(),
        ) {
            let (lo, hi) = if rank(v1) <= rank(v2) { (v1, v2) } else { (v2, v1) };
            // higher reversibility (hi) must be <= restrictive than lower (lo).
            prop_assert!(
                restrictiveness(decide(risk, hi, conf)) <= restrictiveness(decide(risk, lo, conf)),
                "reversibility monotonicity broken"
            );
        }

        // LOWERING confidence never makes the decision less restrictive.
        #[test]
        fn decide_monotone_in_confidence(
            risk in any_level(), rev in any_level(), c1 in any_level(), c2 in any_level(),
        ) {
            let (lo, hi) = if rank(c1) <= rank(c2) { (c1, c2) } else { (c2, c1) };
            prop_assert!(
                restrictiveness(decide(risk, rev, hi)) <= restrictiveness(decide(risk, rev, lo)),
                "confidence monotonicity broken"
            );
        }

        // Hard stop: high risk AND irreversible is ALWAYS Block, whatever the
        // confidence — you can never be sure enough to auto-run an irreversible
        // catastrophe.
        #[test]
        fn decide_high_risk_irreversible_is_always_block(conf in any_level()) {
            prop_assert_eq!(decide(Level::High, Level::Low, conf), Decision::Block);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Level; 3] = [Level::Low, Level::Medium, Level::High];

    #[test]
    fn parse_level_accepts_case_insensitive_synonyms() {
        assert_eq!(parse_level("low"), Some(Level::Low));
        assert_eq!(parse_level("LOW"), Some(Level::Low));
        assert_eq!(parse_level("Medium"), Some(Level::Medium));
        assert_eq!(parse_level("med"), Some(Level::Medium));
        assert_eq!(parse_level("  High  "), Some(Level::High));
        assert_eq!(parse_level("h"), Some(Level::High));
    }

    #[test]
    fn parse_level_rejects_garbage_without_panic() {
        assert_eq!(parse_level(""), None);
        assert_eq!(parse_level("critical"), None);
        assert_eq!(parse_level("2"), None);
    }

    #[test]
    fn from_score_buckets_the_unit_interval_at_documented_thresholds() {
        // Deep in each band.
        assert_eq!(Level::from_score(0.0), Level::Low);
        assert_eq!(Level::from_score(0.2), Level::Low);
        assert_eq!(Level::from_score(0.5), Level::Medium);
        assert_eq!(Level::from_score(0.9), Level::High);
        assert_eq!(Level::from_score(1.0), Level::High);
    }

    #[test]
    fn from_score_boundaries_are_pinned() {
        // Low/Medium boundary at 0.34 (inclusive into Medium).
        assert_eq!(Level::from_score(0.3399), Level::Low);
        assert_eq!(Level::from_score(0.34), Level::Medium);
        // Medium/High boundary at 0.67 (inclusive into High).
        assert_eq!(Level::from_score(0.6699), Level::Medium);
        assert_eq!(Level::from_score(0.67), Level::High);
    }

    #[test]
    fn from_score_clamps_out_of_range_inputs() {
        assert_eq!(Level::from_score(-5.0), Level::Low);
        assert_eq!(Level::from_score(-0.0001), Level::Low);
        assert_eq!(Level::from_score(1.0001), Level::High);
        assert_eq!(Level::from_score(42.0), Level::High);
    }

    #[test]
    fn from_score_nan_is_least_confident_not_a_panic() {
        assert_eq!(Level::from_score(f64::NAN), Level::Low);
    }

    #[test]
    fn from_score_high_score_drives_toward_auto_low_toward_escalate() {
        // Anchors the semantic contract with `decide`: a HIGH calibrated score
        // maps to Level::High which lowers the decision score toward Auto.
        let hi = Level::from_score(0.95);
        let lo = Level::from_score(0.05);
        assert_eq!(hi, Level::High);
        assert_eq!(lo, Level::Low);
        // On the (Medium, Medium) baseline, a calibrated-High confidence Autos
        // while calibrated-Low Escalates (mirrors `delegation_profile_*`).
        assert_eq!(decide(Level::Medium, Level::Medium, hi), Decision::Auto);
        assert_eq!(decide(Level::Medium, Level::Medium, lo), Decision::Escalate);
    }

    #[test]
    fn decision_display_is_exact() {
        assert_eq!(Decision::Auto.to_string(), "auto");
        assert_eq!(Decision::Escalate.to_string(), "escalate");
        assert_eq!(Decision::Block.to_string(), "block");
    }

    #[test]
    fn anchor_block_high_risk_irreversible_regardless_of_confidence() {
        for c in ALL {
            assert_eq!(
                decide(Level::High, Level::Low, c),
                Decision::Block,
                "high risk + irreversible must block at confidence {c:?}"
            );
        }
    }

    #[test]
    fn anchor_auto_trivially_safe_and_reversible() {
        assert_eq!(decide(Level::Low, Level::High, Level::High), Decision::Auto);
        assert_eq!(
            decide(Level::Low, Level::High, Level::Medium),
            Decision::Auto
        );
    }

    #[test]
    fn anchor_escalate_ambiguous_middle() {
        assert_eq!(
            decide(Level::Medium, Level::Medium, Level::Medium),
            Decision::Escalate
        );
    }

    #[test]
    fn delegation_profile_flips_with_confidence() {
        // The routine gate `state autonomy-check` delegates to: autonomous flag
        // supplies confidence on a (Medium, Medium) baseline. High -> Auto,
        // Low -> Escalate (non-Auto). This backs the byte-compat contract.
        assert_eq!(
            decide(Level::Medium, Level::Medium, Level::High),
            Decision::Auto
        );
        assert_eq!(
            decide(Level::Medium, Level::Medium, Level::Low),
            Decision::Escalate
        );
    }

    #[test]
    fn conflict_resolution_never_auto_picks_a_side() {
        // Design 625aa170 B: a conflict/overlap resolution may only Escalate or
        // Block — NEVER Auto (auto pick-side = last-writer-wins). Every input
        // that `decide` would Auto must clamp to Escalate here; nothing else moves.
        for r in ALL {
            for v in ALL {
                for c in ALL {
                    let base = decide(r, v, c);
                    let conflict = decide_conflict_resolution(r, v, c);
                    assert_ne!(
                        conflict,
                        Decision::Auto,
                        "conflict resolution must never Auto (r={r:?} v={v:?} c={c:?})"
                    );
                    match base {
                        Decision::Auto => assert_eq!(
                            conflict,
                            Decision::Escalate,
                            "an Auto verdict must clamp up to Escalate"
                        ),
                        other => assert_eq!(
                            conflict, other,
                            "Escalate/Block verdicts must pass through unchanged"
                        ),
                    }
                }
            }
        }
    }

    #[test]
    fn conflict_resolution_default_posture_is_escalate_on_the_safe_case() {
        // The trivially-safe-and-reversible case that `decide` Autos becomes
        // Escalate under the conflict posture (default = Escalate not Auto).
        assert_eq!(decide(Level::Low, Level::High, Level::High), Decision::Auto);
        assert_eq!(
            decide_conflict_resolution(Level::Low, Level::High, Level::High),
            Decision::Escalate
        );
        // A high-risk irreversible conflict still hard-Blocks.
        assert_eq!(
            decide_conflict_resolution(Level::High, Level::Low, Level::Low),
            Decision::Block
        );
    }

    #[test]
    fn untestable_never_auto_and_never_relaxes_escalate_or_block() {
        // §2 "untestable -> must ask a human": every input for which `decide`
        // Autos must clamp to Escalate under `decide_untestable`; every
        // Escalate/Block input must pass through completely unchanged (it
        // only clamps Auto, it never relaxes). Mirrors
        // `conflict_resolution_never_auto_picks_a_side`.
        for r in ALL {
            for v in ALL {
                for c in ALL {
                    let base = decide(r, v, c);
                    let untestable = decide_untestable(r, v, c);
                    assert_ne!(
                        untestable,
                        Decision::Auto,
                        "decide_untestable must never Auto (r={r:?} v={v:?} c={c:?})"
                    );
                    match base {
                        Decision::Auto => assert_eq!(
                            untestable,
                            Decision::Escalate,
                            "an Auto verdict must clamp up to Escalate (r={r:?} v={v:?} c={c:?})"
                        ),
                        other => assert_eq!(
                            untestable, other,
                            "Escalate/Block verdicts must pass through unchanged \
                             (r={r:?} v={v:?} c={c:?})"
                        ),
                    }
                }
            }
        }
    }

    #[test]
    fn anchor_untestable_escalates_the_trivially_safe_case() {
        // The trivially-safe-and-reversible case that `decide` Autos becomes
        // Escalate under the untestable posture (default = Escalate, not
        // Auto). Mirrors `conflict_resolution_default_posture_is_escalate_on_the_safe_case`.
        assert_eq!(decide(Level::Low, Level::High, Level::High), Decision::Auto);
        assert_eq!(
            decide_untestable(Level::Low, Level::High, Level::High),
            Decision::Escalate
        );
        // A high-risk irreversible untestable decision still hard-Blocks.
        assert_eq!(
            decide_untestable(Level::High, Level::Low, Level::Low),
            Decision::Block
        );
    }

    #[test]
    fn untestable_restrictiveness_is_never_less_than_decide() {
        // decide_untestable's restrictiveness must be >= decide's for every
        // input: it never yields a less restrictive verdict than plain
        // `decide` would.
        for r in ALL {
            for v in ALL {
                for c in ALL {
                    let base = decide(r, v, c).restrictiveness();
                    let untestable = decide_untestable(r, v, c).restrictiveness();
                    assert!(
                        untestable >= base,
                        "decide_untestable must never be less restrictive than decide \
                         (r={r:?} v={v:?} c={c:?}): base={base} untestable={untestable}"
                    );
                }
            }
        }
    }

    #[test]
    fn untestable_is_total_and_never_panics() {
        for r in ALL {
            for v in ALL {
                for c in ALL {
                    let d = decide_untestable(r, v, c);
                    assert!(matches!(
                        d,
                        Decision::Auto | Decision::Escalate | Decision::Block
                    ));
                }
            }
        }
    }

    #[test]
    fn decide_is_total_and_never_panics() {
        for r in ALL {
            for v in ALL {
                for c in ALL {
                    let d = decide(r, v, c);
                    assert!(matches!(
                        d,
                        Decision::Auto | Decision::Escalate | Decision::Block
                    ));
                }
            }
        }
    }

    #[test]
    fn monotone_restrictiveness_in_risk() {
        // Raising risk (holding reversibility, confidence) never lowers restrictiveness.
        for v in ALL {
            for c in ALL {
                let lo = decide(Level::Low, v, c).restrictiveness();
                let mid = decide(Level::Medium, v, c).restrictiveness();
                let hi = decide(Level::High, v, c).restrictiveness();
                assert!(
                    lo <= mid && mid <= hi,
                    "risk not monotone at v={v:?} c={c:?}"
                );
            }
        }
    }

    #[test]
    fn monotone_restrictiveness_as_reversibility_falls() {
        // Lowering reversibility (High -> Low) never lowers restrictiveness.
        for r in ALL {
            for c in ALL {
                let high = decide(r, Level::High, c).restrictiveness();
                let med = decide(r, Level::Medium, c).restrictiveness();
                let low = decide(r, Level::Low, c).restrictiveness();
                assert!(
                    high <= med && med <= low,
                    "reversibility not monotone at r={r:?} c={c:?}"
                );
            }
        }
    }

    #[test]
    fn monotone_restrictiveness_as_confidence_falls() {
        // Lowering confidence (High -> Low) never lowers restrictiveness.
        for r in ALL {
            for v in ALL {
                let high = decide(r, v, Level::High).restrictiveness();
                let med = decide(r, v, Level::Medium).restrictiveness();
                let low = decide(r, v, Level::Low).restrictiveness();
                assert!(
                    high <= med && med <= low,
                    "confidence not monotone at r={r:?} v={v:?}"
                );
            }
        }
    }
}
