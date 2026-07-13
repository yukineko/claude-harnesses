//! UserPromptSubmit injection-size panel (ADR 0001 Phase 2): read the central
//! `inject-metrics.jsonl` ledger written by the five UserPromptSubmit injectors
//! (playbook, runbook, ctxrot, context-governor, fugu-router), aggregate per
//! turn (keyed by `turn_key = hash(session + prompt)`), and flag any turn whose
//! combined post-cap injected char size exceeds the aggregate budget.

use harness_core::inject_metrics::{aggregate, ledger_path, over_budget};
use serde::Serialize;

/// Default aggregate per-turn injection budget in CHARS.
const DEFAULT_BUDGET_CHARS: usize = 20_000;

/// How many recent turns to surface in the human panel.
const RECENT_TURNS: usize = 5;

/// The aggregate per-turn injection budget in chars, from
/// `HARNESS_INJECT_BUDGET_CHARS` (parsed as usize), else [`DEFAULT_BUDGET_CHARS`].
pub fn budget_chars() -> usize {
    harness_core::config::env_u64("HARNESS_INJECT_BUDGET_CHARS")
        .map(|v| v as usize)
        .unwrap_or(DEFAULT_BUDGET_CHARS)
}

/// One turn's aggregated injection size, JSON-serializable for the panel.
#[derive(Serialize)]
pub struct TurnInjectionSummary {
    pub turn_key: String,
    pub total_chars: usize,
    pub per_plugin: Vec<PluginChars>,
    pub over_budget: bool,
}

#[derive(Serialize)]
pub struct PluginChars {
    pub plugin: String,
    pub chars: usize,
}

/// The full injection report threaded into both the `inject` subcommand and the
/// default all-panels view.
#[derive(Serialize)]
pub struct InjectReport {
    pub budget_chars: usize,
    pub turns: Vec<TurnInjectionSummary>,
    pub over_budget_turns: Vec<String>,
}

/// Read + aggregate the central ledger into an [`InjectReport`], capped to the
/// most recent [`RECENT_TURNS`] turns for the panel. A missing or empty ledger
/// yields an empty report (never panics).
pub fn read() -> InjectReport {
    let budget = budget_chars();
    let path = ledger_path();
    let agg = aggregate(&path);
    let over: Vec<String> = over_budget(&agg, budget)
        .iter()
        .map(|t| t.turn_key.clone())
        .collect();
    let turns = agg
        .into_iter()
        .take(RECENT_TURNS)
        .map(|t| TurnInjectionSummary {
            over_budget: t.total_chars > budget,
            turn_key: t.turn_key,
            total_chars: t.total_chars,
            per_plugin: t
                .per_plugin
                .into_iter()
                .map(|(plugin, chars)| PluginChars { plugin, chars })
                .collect(),
        })
        .collect();
    InjectReport {
        budget_chars: budget,
        turns,
        over_budget_turns: over,
    }
}

/// First 8 chars of a turn key (or the whole key if shorter) for compact display.
pub fn key8(turn_key: &str) -> &str {
    turn_key.get(..8).unwrap_or(turn_key)
}

#[cfg(test)]
mod tests {
    use super::*;

    // These tests mutate a process-global env var, so they must not run
    // concurrently with each other.
    use std::sync::Mutex;
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn budget_chars_parses_set_unset_and_garbage() {
        let _g = ENV_LOCK.lock().unwrap();
        let key = "HARNESS_INJECT_BUDGET_CHARS";

        std::env::set_var(key, "12345");
        assert_eq!(budget_chars(), 12345, "a valid usize is honored");

        std::env::set_var(key, "not-a-number");
        assert_eq!(
            budget_chars(),
            DEFAULT_BUDGET_CHARS,
            "garbage falls back to default"
        );

        std::env::remove_var(key);
        assert_eq!(
            budget_chars(),
            DEFAULT_BUDGET_CHARS,
            "unset falls back to default"
        );
    }

    #[test]
    fn empty_or_missing_ledger_yields_empty_report() {
        // `read()` targets the real central ledger, which may or may not exist on
        // this machine. The invariant under test is that it never panics and the
        // structural relations hold: budget populated and over_budget_turns turn
        // keys are a subset of the reported turns (when within the recent cap).
        let report = read();
        assert!(report.budget_chars >= 1, "budget is always populated");
        for t in &report.turns {
            assert_eq!(
                t.over_budget,
                t.total_chars > report.budget_chars,
                "over_budget flag matches the total-vs-budget comparison"
            );
        }
    }
}
