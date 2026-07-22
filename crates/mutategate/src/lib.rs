//! mutategate — a mutation-testing **kill-rate gate** over `cargo-mutants` output.
//!
//! Golden/regression suites prove the code *still does what it did*; they say
//! nothing about whether the tests would *catch a fault* if one were introduced.
//! Mutation testing closes that gap: it injects small faults ("mutants") and
//! checks whether the existing tests fail (the mutant is "caught") or still pass
//! (the mutant "survives" / is "missed"). The fraction of viable mutants caught
//! is the **kill-rate** (a.k.a. mutation score); a low score means the tests are
//! weak regardless of how green they look. (Cf. Meta's ACH and PRIMG,
//! arXiv:2505.05584.)
//!
//! This crate deliberately does **not** implement a mutation engine — it stands on
//! the standard Rust tool [`cargo-mutants`](https://mutants.rs). Its only job is
//! the *gate*: parse `cargo-mutants`'s `outcomes.json`, compute the kill-rate, and
//! decide pass/fail against a threshold. That parse→score→exit-code logic is pure
//! and unit-tested here on fixed sample JSON, so the gate is deterministic and
//! runs without invoking the (slow) mutation engine.
//!
//! ## outcomes.json shape (the bits we rely on)
//! `cargo-mutants` writes `mutants.out/outcomes.json`:
//! ```json
//! {
//!   "outcomes": [
//!     { "scenario": "Baseline",              "summary": "Success" },
//!     { "scenario": { "Mutant": { .. } },    "summary": "CaughtMutant" },
//!     { "scenario": { "Mutant": { .. } },    "summary": "MissedMutant" }
//!   ]
//! }
//! ```
//! `summary` is one of `Success`, `CaughtMutant`, `MissedMutant`, `Timeout`,
//! `Unviable`, `Failure`. The `Baseline` scenario (an unmutated build) is not a
//! mutant and is excluded from the score. We count directly from the `outcomes`
//! array rather than trusting the top-level summary counts, so the score is
//! reproducible from the raw records alone.
#![deny(clippy::panic)]

use serde::Deserialize;

/// Small epsilon so a kill-rate that is exactly the threshold passes despite
/// binary-float rounding (e.g. `0.7999999` when the true value is `0.8`).
const KILL_RATE_EPSILON: f64 = 1e-9;

/// Raw top-level `outcomes.json` document. Only `outcomes` is needed.
#[derive(Debug, Deserialize)]
struct RawLabOutcome {
    #[serde(default)]
    outcomes: Vec<RawScenarioOutcome>,
}

/// Raw per-scenario record. `scenario` is either the string `"Baseline"` or an
/// object `{ "Mutant": { .. } }`; we keep it as untyped JSON and only ask whether
/// it is a mutant.
#[derive(Debug, Deserialize)]
struct RawScenarioOutcome {
    #[serde(default)]
    scenario: serde_json::Value,
    #[serde(default)]
    summary: Option<String>,
}

/// Tallied mutant outcomes (baseline excluded).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MutationSummary {
    /// Mutant detected by a test failure — a "kill".
    pub caught: u64,
    /// Mutant that survived: all tests still passed. This is what the gate
    /// punishes.
    pub missed: u64,
    /// Mutant that made the test run hang past the timeout. Counted as killed:
    /// the mutation produced observable, test-exposed misbehaviour.
    pub timeout: u64,
    /// Mutant that did not compile. Excluded from the denominator — an unviable
    /// mutant carries no signal about test strength.
    pub unviable: u64,
    /// A non-baseline scenario that reported plain `Success` (should not normally
    /// occur for mutants); tracked for completeness, excluded from scoring.
    pub success: u64,
    /// A scenario whose harness itself failed. Tracked, excluded from scoring.
    pub failure: u64,
    /// A scenario whose `summary` value we don't recognise (a forward-compat
    /// state from a newer `cargo-mutants`, or a record with no `summary` at
    /// all). Counted toward `viable()` but NOT toward `killed()` — i.e.
    /// conservatively treated as a surviving mutant — so an unrecognised
    /// state can never silently vanish from the denominator and inflate the
    /// apparent kill-rate (CA-mutategate-01). Also surfaced in the gate's
    /// `reason` text as a warning.
    pub unknown: u64,
}

impl MutationSummary {
    /// Mutants that carry signal about test strength: caught + missed +
    /// timeout + unknown. Unviable mutants (they don't compile) and pure
    /// bookkeeping states (`success`, `failure`) are excluded. `unknown` is
    /// included here — never dropped — so a summary state this crate doesn't
    /// recognise still counts against the score instead of vanishing from it.
    pub fn viable(&self) -> u64 {
        self.caught + self.missed + self.timeout + self.unknown
    }

    /// Mutants the tests killed: caught + timeout.
    pub fn killed(&self) -> u64 {
        self.caught + self.timeout
    }

    /// Kill-rate = killed / viable, or `None` when there are no viable mutants
    /// (an undefined score — treated by the gate as a failure, since a run that
    /// produced no scorable mutants gates nothing).
    pub fn kill_rate(&self) -> Option<f64> {
        let viable = self.viable();
        if viable == 0 {
            None
        } else {
            Some(self.killed() as f64 / viable as f64)
        }
    }
}

/// Count mutant outcomes from raw `outcomes.json` text.
///
/// Baseline scenarios are skipped. Unknown/absent `summary` values are
/// **tracked, not dropped**: they land in `unknown`, which counts toward the
/// viable denominator but not toward killed, so a `cargo-mutants` state this
/// crate doesn't recognise (e.g. a future new state, or a malformed record)
/// can never silently inflate the apparent kill-rate (CA-mutategate-01).
pub fn parse_outcomes(json: &str) -> anyhow::Result<MutationSummary> {
    let lab: RawLabOutcome = serde_json::from_str(json)?;
    let mut s = MutationSummary::default();
    for o in &lab.outcomes {
        if !is_mutant(&o.scenario) {
            continue;
        }
        match o.summary.as_deref() {
            Some("CaughtMutant") => s.caught += 1,
            Some("MissedMutant") => s.missed += 1,
            Some("Timeout") => s.timeout += 1,
            Some("Unviable") => s.unviable += 1,
            Some("Success") => s.success += 1,
            Some("Failure") => s.failure += 1,
            _ => s.unknown += 1,
        }
    }
    Ok(s)
}

/// A scenario is a mutant iff its JSON is an object carrying a `"Mutant"` key
/// (the `Baseline` scenario serialises as the bare string `"Baseline"`).
fn is_mutant(scenario: &serde_json::Value) -> bool {
    scenario.get("Mutant").is_some()
}

/// The gate's verdict for a run.
#[derive(Debug, Clone)]
pub struct GateOutcome {
    pub summary: MutationSummary,
    /// Measured kill-rate, or `None` when no viable mutants were produced.
    pub kill_rate: Option<f64>,
    /// The minimum kill-rate required to pass.
    pub threshold: f64,
    /// Whether the gate passes.
    pub passed: bool,
    /// Human-readable one-line explanation.
    pub reason: String,
}

impl GateOutcome {
    /// The gate's answer in the shared three-valued type
    /// (`harness_core::verdict::Verdict`), derived from the fields above rather
    /// than a private bool. `kill_rate: None` ("no viable mutants produced") is
    /// modeled as [`Verdict::Undetermined`] — the kill-rate genuinely could not
    /// be measured, which is a distinct fact from "measured and below
    /// threshold" — while a measured, below-threshold rate is
    /// [`Verdict::Violation`]. Both block identically
    /// (`Verdict::blocks`/`exit_code`), so this changes no observable
    /// behaviour; it only carries the distinction through the shared type
    /// instead of collapsing it into one `bool`.
    pub fn verdict(&self) -> harness_core::verdict::Verdict {
        if self.kill_rate.is_none() {
            harness_core::verdict::Verdict::undetermined(self.reason.clone())
        } else if self.passed {
            harness_core::verdict::Verdict::from_findings(Vec::new())
        } else {
            harness_core::verdict::Verdict::violation(self.reason.clone())
        }
    }
}

/// Render `a` and `b` (both percentages, e.g. `79.96`) with the fewest decimal
/// places (starting at 1, matching the gate's usual `{:.1}%` display) that
/// still tell them apart. Below-threshold fails are decided on the full-
/// precision `f64`, but `{:.1}%` rounding can make two genuinely different
/// values print identically (e.g. `79.96` and `80.0` both round to `80.0`),
/// which reads as the self-contradictory "80.0% < 80.0%" next to a FAIL
/// verdict (CA-mutategate-02, display-only — never changes the decision).
fn distinguishing_pct_pair(a: f64, b: f64) -> (String, String) {
    for precision in 1..=9 {
        let sa = format!("{:.precision$}", a, precision = precision);
        let sb = format!("{:.precision$}", b, precision = precision);
        if sa != sb {
            return (sa, sb);
        }
    }
    (format!("{a:.9}"), format!("{b:.9}"))
}

/// Decide pass/fail for a tally against a minimum kill-rate.
///
/// Fails when there are no viable mutants (nothing was measured) or when the
/// kill-rate is below `threshold`. The comparison is `>=` with a tiny epsilon so
/// hitting the threshold exactly passes.
pub fn evaluate(summary: MutationSummary, threshold: f64) -> GateOutcome {
    // Surface (never silently drop) any summary states this crate doesn't
    // recognise — a detectable signal alongside the conservative denominator
    // treatment in `MutationSummary::viable()` (CA-mutategate-01).
    let unknown_note = if summary.unknown > 0 {
        format!(
            " [warning: {} mutant(s) had an unrecognised summary state — counted as not-killed, not dropped]",
            summary.unknown
        )
    } else {
        String::new()
    };
    match summary.kill_rate() {
        None => GateOutcome {
            reason: format!(
                "no viable mutants produced ({} unviable, {} caught, {} missed, {} timeout){unknown_note} — nothing to score",
                summary.unviable, summary.caught, summary.missed, summary.timeout
            ),
            kill_rate: None,
            threshold,
            passed: false,
            summary,
        },
        Some(kr) => {
            let passed = kr + KILL_RATE_EPSILON >= threshold;
            let (kr_s, th_s) = distinguishing_pct_pair(kr * 100.0, threshold * 100.0);
            let reason = if passed {
                if kr >= threshold {
                    // Genuinely at or above the threshold.
                    format!(
                        "kill-rate {kr_s}% >= {th_s}% ({} killed / {} viable; {} missed survived){unknown_note}",
                        summary.killed(),
                        summary.viable(),
                        summary.missed,
                    )
                } else {
                    // The kill-rate is a hair *below* the threshold and only the
                    // `KILL_RATE_EPSILON` float-rounding tolerance bridges the gap.
                    // Rendering "{kr_s}% >= {th_s}%" here would claim a smaller
                    // number is at-or-above a larger one — the PASS-direction
                    // mirror of the FAIL contradiction in CA-mutategate-02
                    // (CA-mutategate-03). Phrase it as the tolerance it actually
                    // is instead, so a PASS reason never contradicts itself.
                    format!(
                        "kill-rate {kr_s}% within epsilon ({KILL_RATE_EPSILON:e}) of threshold {th_s}% ({} killed / {} viable; {} missed survived){unknown_note}",
                        summary.killed(),
                        summary.viable(),
                        summary.missed,
                    )
                }
            } else {
                format!(
                    "kill-rate {kr_s}% < {th_s}% ({} missed mutant(s) survived out of {} viable) — tests too weak{unknown_note}",
                    summary.missed,
                    summary.viable(),
                )
            };
            GateOutcome {
                kill_rate: Some(kr),
                threshold,
                passed,
                reason,
                summary,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A realistic (trimmed) `outcomes.json`: one baseline + a mix of mutant
    /// states. Baseline must be ignored; the score is computed from mutants only.
    const SAMPLE: &str = r#"{
      "outcomes": [
        { "scenario": "Baseline", "summary": "Success" },
        { "scenario": { "Mutant": { "file": "src/a.rs", "line": 1 } }, "summary": "CaughtMutant" },
        { "scenario": { "Mutant": { "file": "src/a.rs", "line": 2 } }, "summary": "CaughtMutant" },
        { "scenario": { "Mutant": { "file": "src/a.rs", "line": 3 } }, "summary": "Timeout" },
        { "scenario": { "Mutant": { "file": "src/b.rs", "line": 9 } }, "summary": "MissedMutant" },
        { "scenario": { "Mutant": { "file": "src/b.rs", "line": 4 } }, "summary": "Unviable" }
      ],
      "total_mutants": 5, "caught": 2, "missed": 1, "timeout": 1, "unviable": 1
    }"#;

    #[test]
    fn parse_counts_mutants_and_ignores_baseline() {
        let s = parse_outcomes(SAMPLE).unwrap();
        assert_eq!(s.caught, 2);
        assert_eq!(s.missed, 1);
        assert_eq!(s.timeout, 1);
        assert_eq!(s.unviable, 1);
        assert_eq!(
            s.success, 0,
            "baseline Success must not be counted as a mutant"
        );
    }

    #[test]
    fn viable_and_killed_exclude_unviable() {
        let s = parse_outcomes(SAMPLE).unwrap();
        // viable = caught(2) + missed(1) + timeout(1) = 4  (unviable excluded)
        assert_eq!(s.viable(), 4);
        // killed = caught(2) + timeout(1) = 3
        assert_eq!(s.killed(), 3);
    }

    #[test]
    fn kill_rate_is_killed_over_viable() {
        let s = parse_outcomes(SAMPLE).unwrap();
        assert_eq!(s.kill_rate(), Some(3.0 / 4.0)); // 0.75
    }

    #[test]
    fn gate_fails_when_below_threshold() {
        let s = parse_outcomes(SAMPLE).unwrap();
        let g = evaluate(s, 0.80); // 0.75 < 0.80
        assert!(!g.passed);
        assert!(g.reason.contains("missed"));
    }

    #[test]
    fn gate_passes_when_at_or_above_threshold() {
        let s = parse_outcomes(SAMPLE).unwrap();
        let g = evaluate(s, 0.75); // exactly at threshold -> pass
        assert!(
            g.passed,
            "hitting the threshold exactly must pass: {}",
            g.reason
        );
    }

    #[test]
    fn all_caught_is_full_kill_rate() {
        let json = r#"{ "outcomes": [
            { "scenario": "Baseline", "summary": "Success" },
            { "scenario": { "Mutant": {} }, "summary": "CaughtMutant" },
            { "scenario": { "Mutant": {} }, "summary": "CaughtMutant" }
        ] }"#;
        let s = parse_outcomes(json).unwrap();
        assert_eq!(s.kill_rate(), Some(1.0));
        assert!(evaluate(s, 1.0).passed);
    }

    #[test]
    fn no_viable_mutants_fails_the_gate() {
        // Only an unviable mutant: kill-rate is undefined, gate must fail loudly.
        let json = r#"{ "outcomes": [
            { "scenario": "Baseline", "summary": "Success" },
            { "scenario": { "Mutant": {} }, "summary": "Unviable" }
        ] }"#;
        let s = parse_outcomes(json).unwrap();
        assert_eq!(s.kill_rate(), None);
        let g = evaluate(s, 0.80);
        assert!(!g.passed);
        assert!(g.reason.contains("no viable mutants"));
    }

    #[test]
    fn empty_outcomes_fails_the_gate() {
        let s = parse_outcomes(r#"{ "outcomes": [] }"#).unwrap();
        assert_eq!(s, MutationSummary::default());
        assert!(!evaluate(s, 0.80).passed);
    }

    #[test]
    fn unknown_summary_states_are_tracked_not_dropped() {
        let json = r#"{ "outcomes": [
            { "scenario": { "Mutant": {} }, "summary": "CaughtMutant" },
            { "scenario": { "Mutant": {} }, "summary": "SomeFutureState" },
            { "scenario": { "Mutant": {} } }
        ] }"#;
        let s = parse_outcomes(json).unwrap();
        assert_eq!(s.caught, 1);
        // An unrecognised or absent `summary` must be tracked (CA-mutategate-01),
        // not silently dropped: it still counts toward the viable denominator so
        // it cannot inflate the apparent kill-rate.
        assert_eq!(
            s.unknown, 2,
            "the unrecognised state and the missing-summary record must both be tracked"
        );
        assert_eq!(
            s.viable(),
            3,
            "unknown-summary mutants must count toward viable, not vanish from it"
        );
    }

    // ── CA-mutategate-01: an unrecognised `summary` value must not vanish from
    //    the viable denominator — silently dropping it makes the apparent
    //    kill-rate read higher than reality (fail-open). ─────────────────────
    #[test]
    fn unknown_summary_does_not_inflate_kill_rate() {
        let json = r#"{ "outcomes": [
            { "scenario": { "Mutant": {} }, "summary": "CaughtMutant" },
            { "scenario": { "Mutant": {} }, "summary": "SomeFutureState" }
        ] }"#;
        let s = parse_outcomes(json).unwrap();
        // Old (buggy) behaviour dropped the unknown record entirely, so
        // viable() == 1 and kill_rate() == Some(1.0) — a falsely perfect score.
        // The unknown mutant must count toward viable and NOT be counted killed,
        // so the true (conservative) kill-rate is 1 killed / 2 viable = 0.5.
        assert_eq!(s.viable(), 2);
        assert_eq!(
            s.kill_rate(),
            Some(0.5),
            "an unknown-state mutant must not be silently excluded from the \
             denominator — that would inflate the apparent kill-rate"
        );
    }

    #[test]
    fn malformed_json_is_an_error() {
        assert!(parse_outcomes("not json").is_err());
    }

    // ── verdict(): the shared harness_core::verdict type must agree with
    //    `passed`/`kill_rate` exactly, and both non-clean cases must block. ──

    #[test]
    fn verdict_is_clean_when_passed() {
        let s = parse_outcomes(SAMPLE).unwrap();
        let g = evaluate(s, 0.75); // exactly at threshold -> pass
        assert!(g.passed);
        let v = g.verdict();
        assert!(
            matches!(v, harness_core::verdict::Verdict::Clean(_)),
            "a passing gate must be Clean, got {v:?}"
        );
        assert!(!v.blocks());
    }

    #[test]
    fn verdict_is_violation_when_below_threshold() {
        let s = parse_outcomes(SAMPLE).unwrap();
        let g = evaluate(s, 0.80); // 0.75 < 0.80
        assert!(!g.passed);
        let v = g.verdict();
        assert!(
            matches!(v, harness_core::verdict::Verdict::Violation(_)),
            "a measured below-threshold rate must be a Violation, got {v:?}"
        );
        assert!(v.blocks(), "a Violation must block");
    }

    #[test]
    fn verdict_is_undetermined_when_no_viable_mutants() {
        let json = r#"{ "outcomes": [
            { "scenario": "Baseline", "summary": "Success" },
            { "scenario": { "Mutant": {} }, "summary": "Unviable" }
        ] }"#;
        let s = parse_outcomes(json).unwrap();
        let g = evaluate(s, 0.80);
        assert!(!g.passed);
        assert_eq!(g.kill_rate, None);
        let v = g.verdict();
        assert!(
            matches!(v, harness_core::verdict::Verdict::Undetermined(_)),
            "no viable mutants means the kill-rate could not be measured — \
             Undetermined, not a confirmed Violation; got {v:?}"
        );
        assert!(
            v.blocks(),
            "Undetermined must block exactly like a Violation — no viable \
             mutants must never read as a pass"
        );
    }

    // ── CA-mutategate-02 (display-only): near the threshold, `{:.1}%`
    //    rounding must never print a self-contradictory line like
    //    "80.0% < 80.0%" — the decision is correct, only the text must not
    //    contradict itself. ───────────────────────────────────────────────
    #[test]
    fn near_threshold_fail_message_is_not_self_contradictory() {
        let s = MutationSummary {
            caught: 7996,
            missed: 2004,
            timeout: 0,
            unviable: 0,
            success: 0,
            failure: 0,
            unknown: 0,
        };
        assert_eq!(s.kill_rate(), Some(0.7996));
        let g = evaluate(s, 0.80);
        assert!(!g.passed, "0.7996 must fail an 0.80 threshold");
        assert!(
            !g.reason.contains("80.0% < 80.0%"),
            "reason shows identical numbers on both sides of '<', which \
             contradicts the FAIL decision: {}",
            g.reason
        );
    }

    // ── CA-mutategate-03 (display-only, PASS mirror of -02): when the
    //    `KILL_RATE_EPSILON` tolerance is what bridges a hair-below-threshold
    //    kill-rate, the PASS reason must NOT render "{kr}% >= {threshold}%" with
    //    the smaller kr on the left — that claims a smaller number is at-or-above
    //    a larger one, contradicting itself. ─────────────────────────────────
    #[test]
    fn epsilon_bridged_pass_message_is_not_self_contradictory() {
        // killed = caught(8) + timeout(0) = 8, viable = 8 + missed(2) = 10, so
        // kr = 0.8. Set the threshold a hair ABOVE kr (well under the 1e-9
        // epsilon), so kr is strictly below threshold yet the epsilon still
        // bridges the gap and the gate PASSes.
        let s = MutationSummary {
            caught: 8,
            missed: 2,
            timeout: 0,
            unviable: 0,
            success: 0,
            failure: 0,
            unknown: 0,
        };
        let kr = s.kill_rate().unwrap();
        let threshold = kr + 5e-10; // < KILL_RATE_EPSILON (1e-9) above kr
        assert!(
            kr < threshold,
            "test premise: kr is strictly below threshold"
        );
        let g = evaluate(s, threshold);
        assert!(
            g.passed,
            "the epsilon tolerance must bridge a hair-below-threshold rate: {}",
            g.reason
        );
        // The two rendered percentages the reason would use, computed the same
        // way `evaluate` does. kr_s < th_s numerically (kr is below threshold),
        // so a PASS reason must never assert "{kr_s}% >= {th_s}%".
        let (kr_s, th_s) = distinguishing_pct_pair(kr * 100.0, threshold * 100.0);
        assert!(
            !g.reason.contains(&format!("{kr_s}% >= {th_s}%")),
            "epsilon-bridged PASS reason claims a smaller kill-rate is \
             at-or-above a larger threshold, contradicting itself: {}",
            g.reason
        );
    }
}
