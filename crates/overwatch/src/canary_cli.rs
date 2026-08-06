/// CLI-facing glue for the canary staged-rollout core (`canary.rs`). Keeps the
/// pure planning/gating/rollback logic free of I/O and argument parsing; this
/// module is the thin shell that turns CLI args into calls and prints
/// script-consumable JSON. It executes NOTHING (no rollout, no registry
/// mutation) — it only emits plans and verdicts as data for
/// `scripts/rollout-plugins.sh` to act on under its explicit opt-in flag.
use crate::canary::{
    self, CanaryTarget, GateDecision, HealthGatePolicy, HealthVerdict, PriorInstallState, StagePlan,
};
use crate::store;
use crate::violation::RecurrencePolicy;
use anyhow::Result;
use serde::Serialize;

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

/// A combined canary verdict carrying BOTH the raw-spike and the systemic
/// (fleet-recurrence) signals plus their OR (Problem-2.1). Emitting both lets
/// `scripts/rollout-plugins.sh` trip the gate if EITHER path fires, instead of
/// only reacting to a raw spike. `decision` is the OR: `Rollback` iff either
/// sub-verdict rolls back.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct CombinedVerdict {
    /// The OR of `raw` and `systemic`: `Rollback` iff either advises rollback.
    decision: GateDecision,
    /// The raw-spike sub-verdict (windowed violation count vs threshold).
    raw: HealthVerdict,
    /// The systemic-recurrence sub-verdict (distinct systemic signatures vs
    /// threshold).
    systemic: HealthVerdict,
}

/// The verdict emitted when the violation registry is UNDETERMINED (unreadable
/// or schema-drifted/corrupt) rather than cleanly empty. The canary gate must
/// FAIL CLOSED here: an unknown health signal for a fleet-defense rollout is
/// treated as "roll back / hold", never as "no spike → proceed". Carries a
/// distinct `reason` so `scripts/rollout-plugins.sh` and its logs make the
/// cannot-determine cause explicit instead of it masquerading as a real spike.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct StoreUndeterminedVerdict {
    /// Always `Rollback` — the fail-closed decision.
    decision: GateDecision,
    /// Machine-stable cause tag: the store could not be trusted.
    reason: &'static str,
    /// Human-readable explanation for the rollout log.
    detail: &'static str,
    /// The raw-spike threshold that WOULD have applied (echoed for the log).
    threshold: usize,
    /// The systemic-recurrence threshold that WOULD have applied.
    systemic_threshold: usize,
    /// The window (seconds) that WOULD have applied.
    window_secs: i64,
}

/// Emit the fail-closed rollback verdict for an undetermined violation store and
/// return `Ok(true)` (rollback advised) so `main` exits 3 — the SAME signal the
/// shell keys on for a real spike, so an undetermined store rolls back the stage
/// rather than slipping through the shell's fail-soft "eval error → proceed"
/// branch (which only fires for exit codes OTHER than 3).
fn emit_store_undetermined_rollback(
    threshold: usize,
    systemic_threshold: usize,
    window_secs: i64,
) -> Result<bool> {
    let verdict = StoreUndeterminedVerdict {
        decision: GateDecision::Rollback,
        reason: "violations-store-undetermined",
        detail: "violation registry unreadable or corrupt (schema drift) — \
                 health signal unknown; failing closed and rolling back this stage",
        threshold,
        systemic_threshold,
        window_secs,
    };
    println!("{}", serde_json::to_string_pretty(&verdict)?);
    Ok(true)
}

impl CombinedVerdict {
    /// Combine a raw-spike and a systemic verdict into one, ORing their
    /// decisions (Problem-2.1): the gate trips if EITHER sub-verdict rolls
    /// back. Pure — no I/O — so the OR contract is unit-testable directly.
    fn from_parts(raw: HealthVerdict, systemic: HealthVerdict) -> Self {
        let decision = if raw.should_rollback() || systemic.should_rollback() {
            GateDecision::Rollback
        } else {
            GateDecision::Proceed
        };
        CombinedVerdict {
            decision,
            raw,
            systemic,
        }
    }

    fn should_rollback(&self) -> bool {
        matches!(self.decision, GateDecision::Rollback)
    }
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
/// `since` (Problem-2.2) anchors the registry-mode count to the stage-deploy
/// time: only violations at/after `since` are counted, so pre-deploy noise is
/// not blamed on the canary stage. `None` = no lower bound (unchanged).
///
/// Registry mode emits BOTH a raw-spike and a systemic verdict as a combined
/// JSON (Problem-2.1) and rolls back if EITHER fires — UNLESS the caller
/// explicitly requests only the systemic path via `systemic = true` (kept for
/// backward compatibility with a single-signal caller). The `observed` pure
/// path is unchanged (single verdict), since the shell only uses it for the
/// deterministic dry-run.
///
/// Returns `Ok(true)` when a rollback is advised.
pub fn gate(
    observed: Option<usize>,
    threshold: usize,
    systemic_threshold: usize,
    window_secs: i64,
    systemic: bool,
    now_override: Option<i64>,
    since: Option<i64>,
) -> Result<bool> {
    let policy = HealthGatePolicy {
        max_violations_in_window: threshold,
        window_secs,
    };
    // Problem-2.1b: the systemic (fleet-recurrence) arm gets its OWN, typically
    // lower, threshold so it can trip INDEPENDENTLY of the raw-spike count.
    // With a threshold shared with the raw arm the systemic path was vacuous: a
    // systemic signature requires >= recurrence.threshold occurrences across
    // distinct tasks, so `systemic_count <= raw_count` always — systemic could
    // never trip while raw stayed below the same bar. A dedicated threshold
    // (default 0 = "any fleet-recurring signature since deploy trips") makes the
    // OR in `CombinedVerdict` non-vacuous and realizes genuine fleet-recurrence
    // protection.
    let systemic_policy = HealthGatePolicy {
        max_violations_in_window: systemic_threshold,
        window_secs,
    };

    if let Some(count) = observed {
        // Pure path: decide directly from the supplied count.
        let verdict = canary::decide_from_count(count, policy);
        return print_verdict(&verdict);
    }

    // Registry path: read the violation store for the cwd project, then evaluate.
    // Split out with an explicit `cwd` (the ONLY impurity, `current_dir`, stays
    // here) so the fail-closed wiring is testable against a real on-disk store
    // without touching the process cwd.
    let cwd = std::env::current_dir()?;
    gate_from_registry(&cwd, policy, systemic_policy, systemic, now_override, since)
}

/// The registry arm of [`gate`], with `cwd` injected. Reads the violation store
/// under `cwd` via the FAIL-CLOSED [`store::scan_violations`] and returns
/// `Ok(true)` iff a rollback is advised — including when the store is
/// undetermined. Kept separate from `gate` so a test can drive it against a
/// real temp store (undetermined / absent / clean) and pin the fail-closed
/// decision at the actual fix point, not just its ingredients. The raw
/// thresholds/window are read back off `policy`/`systemic_policy` (they carry
/// the same values) rather than passed again, so the signature stays small.
fn gate_from_registry(
    cwd: &std::path::Path,
    policy: HealthGatePolicy,
    systemic_policy: HealthGatePolicy,
    systemic: bool,
    now_override: Option<i64>,
    since: Option<i64>,
) -> Result<bool> {
    let now = now_override.unwrap_or_else(store::now);
    // Fail CLOSED on an undetermined store. `scan_violations` keeps "absent
    // registry" (a legitimately-empty history — the normal first-deploy case)
    // distinct from "unreadable / schema-drifted" (untrustworthy count). The
    // retired two-valued reader (`read_violations`, since deleted) collapsed BOTH
    // via `.unwrap_or_default()` — plus any per-line parse failure — into an
    // empty vec, so an unreadable/corrupt store
    // scored `observed=0` and PROCEEDED: the exact cannot-determine → allow
    // fail-open this hardening removes. For a fleet-defense GATE, unknown health
    // must roll back, not sail through.
    let events = match store::scan_violations(cwd) {
        store::ViolationScan::Events(v) => v,
        // Genuinely no violations recorded yet — safe to proceed as before.
        store::ViolationScan::Absent => Vec::new(),
        // Unreadable / corrupt — cannot trust the health signal: roll back.
        store::ViolationScan::Undetermined => {
            return emit_store_undetermined_rollback(
                policy.max_violations_in_window,
                systemic_policy.max_violations_in_window,
                policy.window_secs,
            );
        }
    };
    let recurrence = RecurrencePolicy {
        window_secs: policy.window_secs,
        ..RecurrencePolicy::default()
    };

    if systemic {
        // Backward-compatible single-signal path (systemic only).
        let verdict =
            canary::evaluate_health_gate_systemic(&events, now, recurrence, systemic_policy, since);
        return print_verdict(&verdict);
    }

    // Default registry path (Problem-2.1): compute BOTH signals and OR them so
    // rollout honors a raw spike OR a fleet-recurrence (systemic) verdict.
    let raw = canary::evaluate_health_gate(&events, now, policy, since);
    let sys =
        canary::evaluate_health_gate_systemic(&events, now, recurrence, systemic_policy, since);
    let combined = CombinedVerdict::from_parts(raw, sys);
    println!("{}", serde_json::to_string_pretty(&combined)?);
    Ok(combined.should_rollback())
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

    fn verdict(decision: GateDecision, observed: usize) -> HealthVerdict {
        HealthVerdict {
            decision,
            observed_violations: observed,
            threshold: 2,
            window_secs: 900,
        }
    }

    #[test]
    fn combined_verdict_proceeds_only_when_both_proceed() {
        let c = CombinedVerdict::from_parts(
            verdict(GateDecision::Proceed, 1),
            verdict(GateDecision::Proceed, 0),
        );
        assert_eq!(c.decision, GateDecision::Proceed);
        assert!(!c.should_rollback());
    }

    #[test]
    fn combined_verdict_trips_on_raw_spike_only() {
        // Problem-2.1: raw spike rolls back even though systemic proceeds.
        let c = CombinedVerdict::from_parts(
            verdict(GateDecision::Rollback, 5),
            verdict(GateDecision::Proceed, 0),
        );
        assert_eq!(c.decision, GateDecision::Rollback);
        assert!(c.should_rollback());
        assert_eq!(c.raw.decision, GateDecision::Rollback);
        assert_eq!(c.systemic.decision, GateDecision::Proceed);
    }

    // An undetermined violation store must advise ROLLBACK (return Ok(true)),
    // so `main` exits 3 — the SAME signal a real spike uses — rather than
    // returning an error (which the shell's fail-soft branch would turn into
    // PROCEED). This is the fail-CLOSED contract for cannot-determine.
    #[test]
    fn store_undetermined_advises_rollback() {
        let advised = emit_store_undetermined_rollback(2, 0, 900).unwrap();
        assert!(
            advised,
            "an undetermined violation store must advise rollback (fail closed)"
        );
    }

    // The emitted verdict must serialize with decision=rollback and the stable
    // cause tag, so the rollout log makes the cannot-determine cause explicit
    // instead of it masquerading as a counted spike.
    #[test]
    fn store_undetermined_verdict_serializes_as_rollback_with_reason() {
        let v = StoreUndeterminedVerdict {
            decision: GateDecision::Rollback,
            reason: "violations-store-undetermined",
            detail: "x",
            threshold: 2,
            systemic_threshold: 0,
            window_secs: 900,
        };
        let j: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&v).unwrap()).unwrap();
        assert_eq!(j["decision"], "rollback");
        assert_eq!(j["reason"], "violations-store-undetermined");
    }

    /// A process-unique temp dir (pid + monotonic seq, never a wall-clock
    /// second) so back-to-back HOME-locked tests never collide on one dir and
    /// poison each other's append-only violation store.
    fn unique_test_dir(label: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let d = std::env::temp_dir().join(format!(
            "overwatch-canary-test-{}-{}-{}",
            label,
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    // THE regression test for the fix point: drive `gate_from_registry` (the
    // arm `gate` delegates the registry path to) against a REAL on-disk store
    // that is undetermined (one valid event + one schema-drifted line). It must
    // advise ROLLBACK. Reverting the match arm to a two-valued
    // `.unwrap_or_default()` read (as the retired `read_violations` did) would
    // silently drop the bad line, count 1 (< threshold 2) and PROCEED — flipping
    // this assertion. So
    // this test, unlike the isolated scan/emit tests, actually pins the wiring.
    #[test]
    fn gate_from_registry_rolls_back_on_undetermined_store() {
        use crate::violation::{ViolationEvent, ViolationSource};
        // Poison-recovering; see `store::home_lock`.
        let _g = crate::store::HOME_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let prev_home = std::env::var_os("HOME");
        let dir = unique_test_dir("gate-undetermined");
        std::env::set_var("HOME", &dir);

        let ev = ViolationEvent {
            source: ViolationSource::Blastguard,
            signature: "blastguard:rm-rf".into(),
            task_key: "t".into(),
            session_id: "s".into(),
            ts: 1_700_000_000,
            detail: None,
        };
        store::append_violation(&dir, &ev).unwrap();
        let path = store::violations_path(&dir).unwrap();
        {
            use std::io::Write as _;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            writeln!(f, "{{corrupt-schema-drift").unwrap();
        }

        let policy = HealthGatePolicy {
            max_violations_in_window: 2,
            window_secs: 900,
        };
        let sys_policy = HealthGatePolicy {
            max_violations_in_window: 0,
            window_secs: 900,
        };
        let advised =
            gate_from_registry(&dir, policy, sys_policy, false, Some(1_700_000_000), None).unwrap();
        assert!(
            advised,
            "undetermined store must roll back at the gate() fix point (fail closed)"
        );

        match prev_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    // The paired non-regression: a fresh project with NO violation store must
    // PROCEED (Absent → empty), so the fail-closed change does not roll back
    // every first-time canary. Pins that Absent is not conflated with
    // Undetermined at the gate.
    #[test]
    fn gate_from_registry_proceeds_on_absent_store() {
        // Poison-recovering; see `store::home_lock`.
        let _g = crate::store::HOME_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let prev_home = std::env::var_os("HOME");
        let dir = unique_test_dir("gate-absent");
        std::env::set_var("HOME", &dir);

        let policy = HealthGatePolicy {
            max_violations_in_window: 2,
            window_secs: 900,
        };
        let sys_policy = HealthGatePolicy {
            max_violations_in_window: 0,
            window_secs: 900,
        };
        let advised =
            gate_from_registry(&dir, policy, sys_policy, false, Some(1_700_000_000), None).unwrap();
        assert!(
            !advised,
            "a fresh project with no violation store must proceed, not roll back"
        );

        match prev_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn combined_verdict_trips_on_systemic_only() {
        // Problem-2.1: fleet-recurrence rolls back even though raw proceeds —
        // the signal the old rollout path ignored.
        let c = CombinedVerdict::from_parts(
            verdict(GateDecision::Proceed, 1),
            verdict(GateDecision::Rollback, 3),
        );
        assert_eq!(c.decision, GateDecision::Rollback);
        assert!(c.should_rollback());
        assert_eq!(c.raw.decision, GateDecision::Proceed);
        assert_eq!(c.systemic.decision, GateDecision::Rollback);
    }
}
