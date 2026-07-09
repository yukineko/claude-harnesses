/// CLI-facing glue for the canary staged-rollout core (`canary.rs`). Keeps the
/// pure planning/gating/rollback logic free of I/O and argument parsing; this
/// module is the thin shell that turns CLI args into calls and prints
/// script-consumable JSON. It executes NOTHING (no rollout, no registry
/// mutation) — it only emits plans and verdicts as data for
/// `scripts/rollout-plugins.sh` to act on under its explicit opt-in flag.
use crate::canary::{
    self, CanaryTarget, GateDecision, HealthGatePolicy, PriorInstallState, StagePlan,
};
use crate::store;
use crate::violation::RecurrencePolicy;
use anyhow::Result;

/// Parse a comma/space-separated plugin list into an ordered Vec, dropping
/// empty tokens (so trailing commas / stray whitespace are harmless).
fn parse_plugin_list(s: &str) -> Vec<String> {
    s.split([',', ' ', '\n', '\t'])
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect()
}

/// Build a [`StagePlan`] from CLI args and print it as JSON. Exactly one of
/// `stage_size` / `stage_count` is honored; if both are given, `stage_size`
/// wins (documented in the CLI help). Pure: no clock, no I/O.
pub fn plan(plugins: &str, stage_size: Option<usize>, stage_count: Option<usize>) -> Result<()> {
    let list = parse_plugin_list(plugins);
    let plan: StagePlan = match (stage_size, stage_count) {
        (Some(size), _) => canary::plan_stages_by_size(&list, size),
        (None, Some(count)) => canary::plan_stages_by_count(&list, count),
        // Default: the most conservative canary — one plugin per stage.
        (None, None) => canary::plan_stages_by_size(&list, 1),
    };
    println!("{}", serde_json::to_string_pretty(&plan)?);
    Ok(())
}

/// Render a health-gate verdict as JSON and set the process exit intent:
/// a rollback verdict prints `decision: "rollback"` so the caller (shell)
/// can branch on it without parsing prose. Returns whether a rollback is
/// advised, so `main` can also set a non-zero-style signal if desired.
fn print_verdict(verdict: &canary::HealthVerdict) -> Result<bool> {
    println!("{}", serde_json::to_string_pretty(verdict)?);
    Ok(matches!(verdict.decision, GateDecision::Rollback))
}

/// Evaluate the canary health gate.
///
/// Two input modes, both deterministic:
///   * `observed`: a raw violation count supplied directly (fully pure — no
///     clock, no store read). This is the mode the shell script uses so the
///     dry-run test can drive PROCEED vs ROLLBACK with a fixed number.
///   * otherwise: read the item-B violation registry for the cwd project and
///     count violations within the window at `now`. `now` defaults to
///     `store::now()` but only for *reading recency* — it is still passed
///     explicitly into the pure decision function, never read inside it.
///
/// Returns `Ok(true)` when a rollback is advised.
pub fn gate(
    observed: Option<usize>,
    threshold: usize,
    window_secs: i64,
    systemic: bool,
    now_override: Option<i64>,
) -> Result<bool> {
    let policy = HealthGatePolicy {
        max_violations_in_window: threshold,
        window_secs,
    };

    if let Some(count) = observed {
        // Pure path: decide directly from the supplied count.
        let verdict = canary::decide_from_count(count, policy);
        return print_verdict(&verdict);
    }

    // Registry path: read events, then evaluate at an explicit `now`.
    let cwd = std::env::current_dir()?;
    let now = now_override.unwrap_or_else(store::now);
    let events = store::read_violations(&cwd).unwrap_or_default();

    let verdict = if systemic {
        let recurrence = RecurrencePolicy {
            window_secs,
            ..RecurrencePolicy::default()
        };
        canary::evaluate_health_gate_systemic(&events, now, recurrence, policy)
    } else {
        canary::evaluate_health_gate(&events, now, policy)
    };
    print_verdict(&verdict)
}

/// Compute and print a rollback plan as JSON, given prior-install state and
/// canary targets as inline JSON strings (so the shell can pass what it read
/// from `installed_plugins.json`). Pure: computes data only, executes nothing.
pub fn rollback_plan(
    stage_index: usize,
    prior_json: &str,
    canary_targets_json: &str,
) -> Result<()> {
    let prior: Vec<PriorInstallState> = serde_json::from_str(prior_json)?;
    let targets: Vec<CanaryTarget> = serde_json::from_str(canary_targets_json)?;
    let plan = canary::compute_rollback_plan(stage_index, &prior, &targets);
    println!("{}", serde_json::to_string_pretty(&plan)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_plugin_list_handles_commas_and_whitespace() {
        assert_eq!(
            parse_plugin_list("a, b ,c,,  d "),
            vec![
                "a".to_string(),
                "b".to_string(),
                "c".to_string(),
                "d".to_string()
            ]
        );
    }

    #[test]
    fn parse_plugin_list_empty_is_empty() {
        assert!(parse_plugin_list("   ,, ").is_empty());
    }
}
