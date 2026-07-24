//! The mode axis (`fast` / `normal` / `high`): a deterministic clamp applied
//! **after** the policy (`policy::decide`/`decide_bandit`) has already picked
//! a worker/verifier pair, and **before** `policy::downgrade_for_budget`.
//!
//! This module never touches the learning logic in `policy.rs` — it only
//! shifts the *already-decided* [`Decision`] by one tier and re-derives the
//! verifier from the shifted worker via the existing `policy::verifier_model`.
//!
//! Ordering matters (see `apply`'s doc): budget is a hard resource limit and
//! mode is a preference, so `downgrade_for_budget` must run *after* `apply`
//! so budget pressure can still negate a `high` pick. When it does, the
//! negation must be legible in the rationale — `downgrade_for_budget` already
//! appends to (never replaces) the incoming rationale, so as long as `apply`
//! records "mode=high" first, the final rationale shows both the upshift and
//! the downgrade that took it back, instead of the upshift silently
//! disappearing.

use std::fmt;

use clap::ValueEnum;

use crate::policy::{self, Decision};

/// One of the three routing "aggressiveness" presets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "lowercase")]
pub enum Mode {
    /// Shift the worker one tier down (haiku floor); verifier is recomputed
    /// from the shifted worker, then additionally capped at `sonnet` — opus
    /// can never be selected as worker or verifier under `fast`.
    Fast,
    /// Identity — the backward-compatible default. Whatever the policy
    /// already picked stands unchanged.
    Normal,
    /// Shift the worker one tier up (opus ceiling); verifier is recomputed
    /// from the shifted worker.
    High,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Fast => "fast",
            Mode::Normal => "normal",
            Mode::High => "high",
        }
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Strict parse for values that bypass clap's `ValueEnum` validation (the
/// `FUGU_ROUTER_MODE` env var and `config.toml`'s `mode` field, both plain
/// strings). An unrecognised value is an explicit `Err`, **never** silently
/// coerced to `normal` — per CLAUDE.md §3, "cannot determine" must resolve to
/// the restrictive side (an error the caller must surface), not a default
/// that looks like a deliberate choice of "no preference".
pub fn parse_mode(s: &str) -> Result<Mode, String> {
    match s {
        "fast" => Ok(Mode::Fast),
        "normal" => Ok(Mode::Normal),
        "high" => Ok(Mode::High),
        other => Err(format!(
            "invalid fugu-router mode {other:?} (expected one of: fast, normal, high)"
        )),
    }
}

/// Resolve the effective mode: CLI flag > env `FUGU_ROUTER_MODE` > config.toml
/// `mode` > default `normal`. An invalid env or config value is an explicit
/// error (the caller must turn this into a non-zero exit) — it must never
/// silently fall through to `normal`, since that would make a typo
/// indistinguishable from an intentional "no preference".
pub fn resolve(
    cli: Option<Mode>,
    env_value: Option<String>,
    config_value: Option<&str>,
) -> Result<Mode, String> {
    if let Some(m) = cli {
        return Ok(m);
    }
    if let Some(v) = env_value {
        return parse_mode(&v).map_err(|e| format!("FUGU_ROUTER_MODE: {e}"));
    }
    if let Some(v) = config_value {
        return parse_mode(v).map_err(|e| format!("config.toml `mode`: {e}"));
    }
    Ok(Mode::Normal)
}

/// Apply the mode clamp to an already-decided [`Decision`].
///
/// A `gated` decision (human-approved opus/opus) is a **no-op under every
/// mode** — mirrors `policy::downgrade_for_budget`'s own `gated` no-op — and
/// is returned completely unchanged (rationale included), since a gated task
/// was never auto-routed in the first place.
///
/// Otherwise the worker is shifted one tier (`fast` down / `high` up /
/// `normal` unchanged) and the verifier is **recomputed from the shifted
/// worker** via `policy::verifier_model` (never carried over unchanged) —
/// under `fast` the recomputed verifier is additionally capped at `sonnet` so
/// opus can never appear as fast-mode's verifier either. The rationale always
/// records the mode applied and the resulting worker/verifier shift, so a
/// mode's effect (or lack of one) is never silent.
pub fn apply(d: Decision, mode: Mode, class: &str, title: &str) -> Decision {
    if d.basis == "gated" {
        return d;
    }
    match mode {
        Mode::Normal => {
            let rationale = format!(
                "{} | mode=normal: unchanged (worker={}, verifier={})",
                d.rationale, d.worker_model, d.verifier_model
            );
            Decision { rationale, ..d }
        }
        Mode::Fast | Mode::High => {
            let new_worker = match mode {
                Mode::Fast => policy::one_tier_down(&d.worker_model).to_string(),
                Mode::High => policy::one_tier_up(&d.worker_model).to_string(),
                Mode::Normal => unreachable!("handled above"),
            };
            let mut new_verifier = policy::verifier_model(&new_worker, class, title).to_string();
            let mut cap_note = "";
            if mode == Mode::Fast && new_verifier == "opus" {
                new_verifier = "sonnet".to_string();
                cap_note = " (verifier capped at sonnet — fast never yields opus)";
            }
            let rationale = format!(
                "{} | mode={mode}: worker {}→{new_worker}, verifier {}→{new_verifier}{cap_note}",
                d.rationale, d.worker_model, d.verifier_model,
            );
            Decision {
                worker_model: new_worker,
                verifier_model: new_verifier,
                rationale,
                ..d
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_decision(worker: &str, verifier: &str, basis: &'static str) -> Decision {
        Decision {
            worker_model: worker.into(),
            verifier_model: verifier.into(),
            basis,
            confidence: "high",
            neighbors: 0,
            rationale: "base rationale".into(),
        }
    }

    #[test]
    fn normal_is_identity_for_every_tier_pick() {
        for (w, v) in [("haiku", "sonnet"), ("sonnet", "haiku"), ("opus", "sonnet")] {
            let d = base_decision(w, v, "learned");
            let out = apply(d, Mode::Normal, "parallel", "add a field");
            assert_eq!(out.worker_model, w);
            assert_eq!(out.verifier_model, v);
            assert!(out.rationale.contains("mode=normal"));
        }
    }

    /// The worked example from the task spec: policy pick = sonnet.
    #[test]
    fn worked_example_sonnet_pick_across_all_three_modes() {
        let low_stakes_verifier_for_sonnet = "haiku"; // verifier_model("sonnet", "parallel", ..)
        let d = base_decision("sonnet", low_stakes_verifier_for_sonnet, "learned");

        let fast = apply(d.clone(), Mode::Fast, "parallel", "add a field");
        assert_eq!(fast.worker_model, "haiku");
        assert_eq!(fast.verifier_model, "sonnet");

        let normal = apply(d.clone(), Mode::Normal, "parallel", "add a field");
        assert_eq!(normal.worker_model, "sonnet");
        assert_eq!(normal.verifier_model, "haiku");

        let high = apply(d, Mode::High, "parallel", "add a field");
        assert_eq!(high.worker_model, "opus");
        assert_eq!(high.verifier_model, "sonnet");
    }

    #[test]
    fn all_modes_against_haiku_pick() {
        let d = base_decision("haiku", "sonnet", "learned");

        let fast = apply(d.clone(), Mode::Fast, "parallel", "add a field");
        assert_eq!(fast.worker_model, "haiku"); // already floor
        assert_eq!(fast.verifier_model, "sonnet"); // verifier_model(haiku,..)

        let normal = apply(d.clone(), Mode::Normal, "parallel", "add a field");
        assert_eq!(normal.worker_model, "haiku");
        assert_eq!(normal.verifier_model, "sonnet");

        let high = apply(d, Mode::High, "parallel", "add a field");
        assert_eq!(high.worker_model, "sonnet");
        assert_eq!(high.verifier_model, "haiku"); // verifier_model(sonnet,..) low-stakes
    }

    #[test]
    fn all_modes_against_opus_pick() {
        let d = base_decision("opus", "sonnet", "learned");

        let fast = apply(d.clone(), Mode::Fast, "parallel", "add a field");
        assert_eq!(fast.worker_model, "sonnet");
        assert_eq!(fast.verifier_model, "haiku"); // verifier_model(sonnet,..) low-stakes

        let normal = apply(d.clone(), Mode::Normal, "parallel", "add a field");
        assert_eq!(normal.worker_model, "opus");
        assert_eq!(normal.verifier_model, "sonnet");

        let high = apply(d, Mode::High, "parallel", "add a field");
        assert_eq!(high.worker_model, "opus"); // ceiling
        assert_eq!(high.verifier_model, "sonnet"); // verifier_model(opus,..)
    }

    #[test]
    fn fast_never_yields_opus_for_worker_or_verifier() {
        // High-stakes (serial + design keyword): verifier_model would want
        // opus for a sonnet worker, but fast must cap it at sonnet.
        let d = base_decision("opus", "sonnet", "learned");
        let out = apply(d, Mode::Fast, "serial", "redesign the auth architecture");
        assert_ne!(out.worker_model, "opus");
        assert_ne!(out.verifier_model, "opus");
        assert!(out.rationale.contains("capped at sonnet"));
    }

    #[test]
    fn high_never_exceeds_opus() {
        let d = base_decision("opus", "sonnet", "learned");
        let out = apply(d, Mode::High, "parallel", "add a field");
        assert_eq!(out.worker_model, "opus");
        assert_eq!(out.verifier_model, "sonnet");
    }

    #[test]
    fn gated_decision_is_unchanged_under_fast_and_high() {
        let d = base_decision("opus", "opus", "gated");

        let fast = apply(d.clone(), Mode::Fast, "gated", "deploy to prod");
        assert_eq!(fast.worker_model, "opus");
        assert_eq!(fast.verifier_model, "opus");
        assert_eq!(fast.rationale, d.rationale);

        let high = apply(d.clone(), Mode::High, "gated", "deploy to prod");
        assert_eq!(high.worker_model, "opus");
        assert_eq!(high.verifier_model, "opus");
        assert_eq!(high.rationale, d.rationale);

        let normal = apply(d.clone(), Mode::Normal, "gated", "deploy to prod");
        assert_eq!(normal.rationale, d.rationale);
    }

    #[test]
    fn budget_downgrade_after_mode_high_negates_and_records_it_in_rationale() {
        let d = base_decision("sonnet", "haiku", "learned");
        let highd = apply(d, Mode::High, "parallel", "add a field");
        assert_eq!(highd.worker_model, "opus"); // high shifted sonnet -> opus

        // Ordering under test: mode clamp FIRST, downgrade_for_budget LAST.
        let downgraded = policy::downgrade_for_budget(highd);

        // Budget wins: the high-mode opus pick got shaved back down.
        assert_ne!(
            downgraded.worker_model, "opus",
            "budget pressure must be able to negate a high-mode upshift"
        );
        // The negation must be legible, not a silent disappearance.
        assert!(downgraded.rationale.contains("mode=high"));
        assert!(downgraded.rationale.contains("budget pressure"));
    }

    #[test]
    fn invalid_env_mode_is_an_explicit_error_not_a_default() {
        let err = resolve(None, Some("turbo".to_string()), None)
            .expect_err("an invalid FUGU_ROUTER_MODE must error, not silently become normal");
        assert!(err.contains("turbo"));
        assert!(err.contains("FUGU_ROUTER_MODE"));
    }

    #[test]
    fn invalid_config_mode_is_an_explicit_error_not_a_default() {
        let err = resolve(None, None, Some("turbo"))
            .expect_err("an invalid config `mode` must error, not silently become normal");
        assert!(err.contains("turbo"));
        assert!(err.contains("config.toml"));
    }

    #[test]
    fn precedence_cli_over_env_over_config_over_default() {
        assert_eq!(
            resolve(Some(Mode::High), Some("fast".into()), Some("fast")).unwrap(),
            Mode::High
        );
        assert_eq!(
            resolve(None, Some("fast".into()), Some("high")).unwrap(),
            Mode::Fast
        );
        assert_eq!(resolve(None, None, Some("high")).unwrap(), Mode::High);
        assert_eq!(resolve(None, None, None).unwrap(), Mode::Normal);
    }
}
