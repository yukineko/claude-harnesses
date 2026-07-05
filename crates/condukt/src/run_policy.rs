//! RUN-POLICY gate: deterministic verify→docker→ship stage selection.
//!
//! Phase 6 (verification) needs a deterministic answer to "what's the next
//! stage?" given three cheap signals: did the cheap (host-side) verify pass,
//! how much does the current runtime diverge from production, and how risky
//! is the change. Escalating straight to a Docker re-verify (or straight to
//! ship) for every task burns time/tokens on low-risk, low-divergence work;
//! but skipping the container signal for a task that touches
//! production-divergent behavior risks shipping on a false-positive cheap
//! pass. This module is the deterministic core only: given the three graded
//! signals it computes the next stage. No LLM call, no I/O, no clock — pure
//! function of its arguments (this purity guarantee is unchanged).
//!
//! Acting on the verdict has TWO consumers. (1) A deterministic in-code
//! consumer, [`crate::verify::run_policy_gate`], mechanically routes the
//! `EscalateDocker` verdict (and only that verdict) to a container launch via
//! `condukt verify launch --run-policy` — no LLM, the escalation DECISION stays
//! pure Rust and only the injected container run does I/O. (2) The `/condukt`
//! SKILL orchestration may still act on the verdict for the stages the gate
//! does not automate (proceeding to the Phase 8 ship stage, or asking the
//! human). Either way this module stays the deterministic core only.
//!
//! The `condukt run-policy decide` CLI subcommand (see `main.rs`) exposes
//! this to the `/condukt` skill's Phase 6, mirroring how `condukt replan
//! handoff` exposes `replan::decide_replan`: the DECISION here is
//! deterministic Rust, the actual verification/shipping work stays the
//! interpreter/worker/verifier's job.

use serde::{Deserialize, Serialize};

/// The four deterministic verdicts the RUN-POLICY gate can emit.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")] // -> "verify_only" | "escalate_docker" | "escalate_ship" | "ask_human"
pub enum RunPolicyVerdict {
    /// The cheap verify is a trustworthy-enough signal on its own: hold here,
    /// skip the Docker re-verify, and do NOT auto-ship.
    VerifyOnly,
    /// Escalate to a containerized re-verify (`condukt verify launch --docker`)
    /// for a trustworthy signal before deciding ship vs ask-human.
    EscalateDocker,
    /// Cheap verify is green, divergence is low, and the change is low-risk:
    /// proceed to the ship stage (still user-gated at Phase 8).
    EscalateShip,
    /// No trustworthy automated signal, or the change/divergence is risky
    /// enough that a human should decide: escalate to the human rather than
    /// auto-deciding.
    AskHuman,
}

/// The deterministic decision emitted by [`decide_run_policy`]. Serialized as
/// the output of `condukt run-policy decide`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunPolicyDecision {
    /// The chosen verdict.
    pub verdict: RunPolicyVerdict,
    /// Human-readable explanation of the decision (mirrors
    /// `replan::FailureClassification::reason`'s design: the FORMATTING is
    /// deterministic Rust, the fix/ship DECISION stays with the LLM/human).
    pub reason: String,
    /// Canonical echo of the parsed `cheap_verify` input (e.g. "pass").
    pub cheap_verify: String,
    /// Canonical echo of the parsed `divergence` input (e.g. "high").
    pub divergence: String,
    /// Canonical echo of the parsed `change_risk` input (e.g. "low").
    pub change_risk: String,
}

/// Graded cheap-verify outcome. Unrecognized input fail-softs to `Unknown`
/// (the safest value: no trustworthy signal → ask the human rather than
/// silently proceeding).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CheapVerify {
    Pass,
    Fail,
    Unknown,
}

impl CheapVerify {
    fn as_str(self) -> &'static str {
        match self {
            CheapVerify::Pass => "pass",
            CheapVerify::Fail => "fail",
            CheapVerify::Unknown => "unknown",
        }
    }
}

/// Graded production-divergence level. Unrecognized input fail-softs to
/// `High` (the safest value: unknown divergence is treated as if it were
/// maximally divergent, preferring a container re-verify over a silent ship).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Divergence {
    Low,
    Medium,
    High,
}

impl Divergence {
    fn as_str(self) -> &'static str {
        match self {
            Divergence::Low => "low",
            Divergence::Medium => "medium",
            Divergence::High => "high",
        }
    }
}

/// Graded change-risk level. Unrecognized input fail-softs to `High` (the
/// safest value: an unclassified change is treated as risky, preferring
/// human review over a silent ship).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChangeRisk {
    Low,
    Medium,
    High,
}

impl ChangeRisk {
    fn as_str(self) -> &'static str {
        match self {
            ChangeRisk::Low => "low",
            ChangeRisk::Medium => "medium",
            ChangeRisk::High => "high",
        }
    }
}

/// Parse a cheap-verify string case-insensitively. Fail-soft: unrecognized
/// input → `Unknown` (never panics).
fn parse_cheap_verify(s: &str) -> CheapVerify {
    match s.trim().to_lowercase().as_str() {
        "pass" => CheapVerify::Pass,
        "fail" => CheapVerify::Fail,
        _ => CheapVerify::Unknown,
    }
}

/// Parse a divergence string case-insensitively. Fail-soft: unrecognized
/// input → `High` (the safest value; never panics).
fn parse_divergence(s: &str) -> Divergence {
    match s.trim().to_lowercase().as_str() {
        "low" => Divergence::Low,
        "medium" => Divergence::Medium,
        "high" => Divergence::High,
        _ => Divergence::High,
    }
}

/// Parse a change-risk string case-insensitively. Fail-soft: unrecognized
/// input → `High` (the safest value; never panics).
fn parse_change_risk(s: &str) -> ChangeRisk {
    match s.trim().to_lowercase().as_str() {
        "low" => ChangeRisk::Low,
        "medium" => ChangeRisk::Medium,
        "high" => ChangeRisk::High,
        _ => ChangeRisk::High,
    }
}

/// Decide the next verify→docker→ship pipeline stage from a cheap-verify
/// result, a production-divergence level, and a change-risk level.
///
/// Pure and deterministic: no LLM, no network, no filesystem, no clock.
/// Fail-soft parsing to the SAFEST value on unrecognized input: unrecognized
/// `cheap_verify` → `Unknown` (no trustworthy signal), unrecognized
/// `divergence`/`change_risk` → `High` (prefer a container re-verify or human
/// review over silently shipping). Never panics.
///
/// Decision table:
/// ```text
/// cheap_verify=Fail:
///     divergence=High           -> EscalateDocker
///     divergence=Low|Medium     -> AskHuman
/// cheap_verify=Unknown          -> AskHuman
/// cheap_verify=Pass:
///     divergence=High           -> EscalateDocker
///     divergence=Medium         -> AskHuman
///     divergence=Low:
///         change_risk=Low       -> EscalateShip
///         change_risk=Medium    -> VerifyOnly
///         change_risk=High      -> AskHuman
/// ```
pub fn decide_run_policy(
    cheap_verify: &str,
    divergence: &str,
    change_risk: &str,
) -> RunPolicyDecision {
    let cv = parse_cheap_verify(cheap_verify);
    let dv = parse_divergence(divergence);
    let cr = parse_change_risk(change_risk);

    let (verdict, reason) = match cv {
        CheapVerify::Fail => match dv {
            Divergence::High => (
                RunPolicyVerdict::EscalateDocker,
                "cheap verify failed and the runtime is highly divergent from production — a \
                 containerized re-verify gives a trustworthy signal before deciding what to do \
                 next"
                    .to_string(),
            ),
            Divergence::Low | Divergence::Medium => (
                RunPolicyVerdict::AskHuman,
                "cheap verify failed with low/medium production divergence — this looks like a \
                 genuine failure, not an environment artifact, so it is returned to the human \
                 rather than auto-escalated"
                    .to_string(),
            ),
        },
        CheapVerify::Unknown => (
            RunPolicyVerdict::AskHuman,
            "cheap_verify is unrecognized/unavailable — no trustworthy automated signal, so the \
             decision is escalated to the human"
                .to_string(),
        ),
        CheapVerify::Pass => match dv {
            Divergence::High => (
                RunPolicyVerdict::EscalateDocker,
                "cheap verify passed but the runtime is highly divergent from production — \
                 confirm the result in a container before trusting it"
                    .to_string(),
            ),
            Divergence::Medium => (
                RunPolicyVerdict::AskHuman,
                "cheap verify passed but production divergence is at the medium threshold — \
                 ambiguous enough to ask the human rather than auto-decide"
                    .to_string(),
            ),
            Divergence::Low => match cr {
                ChangeRisk::Low => (
                    RunPolicyVerdict::EscalateShip,
                    "cheap verify passed, low production divergence, and a low-risk change — \
                     safe to proceed to the ship stage"
                        .to_string(),
                ),
                ChangeRisk::Medium => (
                    RunPolicyVerdict::VerifyOnly,
                    "cheap verify passed with low production divergence but a medium-risk \
                     change — hold here: verified, skip the Docker re-verify, but do not \
                     auto-ship"
                        .to_string(),
                ),
                ChangeRisk::High => (
                    RunPolicyVerdict::AskHuman,
                    "cheap verify passed with low production divergence but a high-risk change \
                     — a human should sign off before shipping"
                        .to_string(),
                ),
            },
        },
    };

    RunPolicyDecision {
        verdict,
        reason,
        cheap_verify: cv.as_str().to_string(),
        divergence: dv.as_str().to_string(),
        change_risk: cr.as_str().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── each of the four verdicts reachable ────────────────────────────────

    #[test]
    fn pass_low_medium_is_verify_only() {
        let d = decide_run_policy("pass", "low", "medium");
        assert_eq!(d.verdict, RunPolicyVerdict::VerifyOnly);
        assert_eq!(d.cheap_verify, "pass");
        assert_eq!(d.divergence, "low");
        assert_eq!(d.change_risk, "medium");
    }

    #[test]
    fn pass_high_low_is_escalate_docker() {
        let d = decide_run_policy("pass", "high", "low");
        assert_eq!(d.verdict, RunPolicyVerdict::EscalateDocker);
    }

    #[test]
    fn pass_low_low_is_escalate_ship() {
        let d = decide_run_policy("pass", "low", "low");
        assert_eq!(d.verdict, RunPolicyVerdict::EscalateShip);
    }

    #[test]
    fn pass_medium_low_is_ask_human() {
        let d = decide_run_policy("pass", "medium", "low");
        assert_eq!(d.verdict, RunPolicyVerdict::AskHuman);
    }

    // ── fail / unknown branches ─────────────────────────────────────────────

    #[test]
    fn fail_high_low_is_escalate_docker() {
        let d = decide_run_policy("fail", "high", "low");
        assert_eq!(d.verdict, RunPolicyVerdict::EscalateDocker);
    }

    #[test]
    fn fail_low_low_is_ask_human() {
        let d = decide_run_policy("fail", "low", "low");
        assert_eq!(d.verdict, RunPolicyVerdict::AskHuman);
    }

    #[test]
    fn unknown_cheap_verify_is_ask_human() {
        let d = decide_run_policy("bogus", "low", "low");
        assert_eq!(d.verdict, RunPolicyVerdict::AskHuman);
        assert_eq!(d.cheap_verify, "unknown");
    }

    // ── fail-soft parsing to the safest value ───────────────────────────────

    #[test]
    fn unknown_divergence_and_risk_fail_soft_to_high() {
        // unrecognized divergence -> High -> EscalateDocker (regardless of the
        // also-unrecognized change_risk, since High divergence short-circuits
        // before change_risk is consulted).
        let d = decide_run_policy("pass", "garbage", "garbage");
        assert_eq!(d.verdict, RunPolicyVerdict::EscalateDocker);
        assert_eq!(d.divergence, "high");
        assert_eq!(d.change_risk, "high");
    }

    #[test]
    fn unknown_change_risk_alone_fails_soft_to_high() {
        let d = decide_run_policy("pass", "low", "garbage");
        assert_eq!(d.verdict, RunPolicyVerdict::AskHuman);
        assert_eq!(d.change_risk, "high");
    }

    #[test]
    fn case_insensitive_parsing() {
        let d = decide_run_policy("PASS", "LOW", "LOW");
        assert_eq!(d.verdict, RunPolicyVerdict::EscalateShip);
    }

    // ── determinism ──────────────────────────────────────────────────────

    #[test]
    fn decide_run_policy_is_deterministic() {
        let d1 = decide_run_policy("pass", "high", "low");
        let d2 = decide_run_policy("pass", "high", "low");
        assert_eq!(d1, d2);
    }

    #[test]
    fn empty_inputs_never_panic() {
        let d = decide_run_policy("", "", "");
        assert_eq!(d.verdict, RunPolicyVerdict::AskHuman);
    }

    #[test]
    fn garbage_inputs_never_panic() {
        let garbage = "\u{0}\t!@#$%^&*()_+{}|:<>?~`-=[]\\;',./\u{1b}[31m";
        let d = decide_run_policy(garbage, garbage, garbage);
        // garbage cheap_verify -> Unknown -> AskHuman (no trustworthy signal at all).
        assert_eq!(d.verdict, RunPolicyVerdict::AskHuman);
    }

    // ── serde ────────────────────────────────────────────────────────────

    #[test]
    fn escalate_docker_serializes_to_snake_case() {
        let json = serde_json::to_string(&RunPolicyVerdict::EscalateDocker).unwrap();
        assert_eq!(json, "\"escalate_docker\"");
    }

    #[test]
    fn all_verdicts_serialize_to_expected_snake_case() {
        assert_eq!(
            serde_json::to_string(&RunPolicyVerdict::VerifyOnly).unwrap(),
            "\"verify_only\""
        );
        assert_eq!(
            serde_json::to_string(&RunPolicyVerdict::EscalateShip).unwrap(),
            "\"escalate_ship\""
        );
        assert_eq!(
            serde_json::to_string(&RunPolicyVerdict::AskHuman).unwrap(),
            "\"ask_human\""
        );
    }
}
