/// Opt-in **canary staged rollout** planning for the one deploy-like operation
/// this repo has: `scripts/rollout-plugins.sh` (which copies `crates/<name>/`
/// into the live plugin cache and repoints the installed-plugins registry).
///
/// This module is the *deterministic core* of that canary machinery. It does
/// three pure things and executes NONE of them:
///
///   1. **stage planning** — split an ordered plugin set into ordered stages
///      (configurable stage size / count), so a rollout can reach only a
///      fraction of the fleet first and be observed before proceeding.
///   2. **health gate** — given the item-B gate-violation registry (folded to
///      a per-window violation rate) plus a configurable threshold, decide
///      `Proceed` vs `Rollback` for a stage. Like the item-B recurrence
///      logic, `now` is an explicit argument — there is NO wall-clock read on
///      the decision path, which keeps the verdict reproducible and testable.
///   3. **rollback plan** — compute, as data, what a rollback would restore:
///      the prior version dir to re-point to and the registry pointer to
///      revert. It does NOT touch the filesystem or the registry; the shell
///      script is responsible for any actual mutation, and only under the
///      explicit opt-in flag.
///
/// Everything here is pure and deterministic: no I/O, no randomness, no
/// implicit clock. This mirrors `violation.rs`'s posture so the canary
/// verdict is judged the same way overwatch already judges liveness and
/// recurrence — by injected timestamps compared against a window.
use crate::violation::{detect_recurrence, RecurrencePolicy, ViolationEvent};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Stage planning
// ---------------------------------------------------------------------------

/// One canary stage: a contiguous slice of the ordered plugin set that is
/// rolled out (and then observed) together before the next stage proceeds.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Stage {
    /// 0-based index of this stage within the plan.
    pub index: usize,
    /// The plugin names in this stage, in the original given order.
    pub plugins: Vec<String>,
}

/// An ordered set of canary stages covering the full plugin set exactly once.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StagePlan {
    /// The stages, in rollout order.
    pub stages: Vec<Stage>,
}

impl StagePlan {
    /// Total number of stages.
    pub fn len(&self) -> usize {
        self.stages.len()
    }

    /// Whether the plan has no stages (i.e. no plugins to roll out).
    pub fn is_empty(&self) -> bool {
        self.stages.is_empty()
    }
}

/// Split an ordered plugin set into stages of at most `stage_size` plugins
/// each, preserving order. A `stage_size` of 0 is treated as 1 (never panic /
/// divide-by-zero; degrade to one-plugin-per-stage, the most conservative
/// canary). An empty plugin set yields an empty plan.
///
/// Deterministic: same input → same output, no clock, no randomness.
pub fn plan_stages_by_size(plugins: &[String], stage_size: usize) -> StagePlan {
    let size = stage_size.max(1);
    let stages = plugins
        .chunks(size)
        .enumerate()
        .map(|(index, chunk)| Stage {
            index,
            plugins: chunk.to_vec(),
        })
        .collect();
    StagePlan { stages }
}

/// Split an ordered plugin set into exactly `stage_count` stages (the last
/// stages absorb the remainder as evenly as possible), preserving order. A
/// `stage_count` of 0 is treated as 1. If there are fewer plugins than
/// stages, empty trailing stages are omitted (you never get an empty stage).
///
/// Deterministic: same input → same output.
pub fn plan_stages_by_count(plugins: &[String], stage_count: usize) -> StagePlan {
    let count = stage_count.max(1);
    if plugins.is_empty() {
        return StagePlan { stages: Vec::new() };
    }
    let n = plugins.len();
    let base = n / count;
    let rem = n % count;

    let mut stages = Vec::new();
    let mut start = 0usize;
    for i in 0..count {
        // The first `rem` stages get one extra plugin so the split is even.
        let this = base + usize::from(i < rem);
        if this == 0 {
            continue; // more stages than plugins → omit empties
        }
        let end = start + this;
        stages.push(Stage {
            index: stages.len(),
            plugins: plugins[start..end].to_vec(),
        });
        start = end;
    }
    StagePlan { stages }
}

// ---------------------------------------------------------------------------
// Health gate
// ---------------------------------------------------------------------------

/// The decision a health gate renders for a stage.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum GateDecision {
    /// The observed violation rate is within tolerance — advance to the next
    /// stage.
    Proceed,
    /// The observed violation rate exceeded the configured threshold — halt
    /// and roll back this stage.
    Rollback,
}

/// Configuration for the canary health gate. All fields are configurable per
/// the design: what counts as an acceptable spike varies by project.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct HealthGatePolicy {
    /// Maximum tolerated violations-per-window. If the observed count within
    /// the window is strictly greater than this, the gate says `Rollback`.
    /// Expressed as a rate (violations within `window_secs`) so it composes
    /// with the item-B violation registry directly.
    pub max_violations_in_window: usize,
    /// Sliding window size in seconds. Only violations with
    /// `now - ts <= window_secs` (and not in the future) are counted. No
    /// wall-clock read — `now` is supplied by the caller.
    pub window_secs: i64,
}

impl Default for HealthGatePolicy {
    fn default() -> Self {
        // Conservative default: any more than 2 violations within a 15-minute
        // window after a stage is treated as a spike worth rolling back.
        Self {
            max_violations_in_window: 2,
            window_secs: 900,
        }
    }
}

/// The rendered verdict of a health gate, carrying the decision plus the
/// evidence used, so callers (and the shell script) can log *why* without
/// re-deriving it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HealthVerdict {
    /// Proceed or Rollback.
    pub decision: GateDecision,
    /// Number of violations counted within the window at `now`.
    pub observed_violations: usize,
    /// The threshold that was compared against.
    pub threshold: usize,
    /// The window (seconds) that was applied.
    pub window_secs: i64,
}

impl HealthVerdict {
    /// True iff the gate says to roll this stage back.
    pub fn should_rollback(&self) -> bool {
        matches!(self.decision, GateDecision::Rollback)
    }
}

/// Decide `Proceed` vs `Rollback` from a raw observed violation count.
///
/// Pure and deterministic: the decision is purely `observed > threshold`.
/// This is the lowest-level gate; `evaluate_health_gate` layers windowed
/// counting over the item-B registry on top of it.
pub fn decide_from_count(observed_violations: usize, policy: HealthGatePolicy) -> HealthVerdict {
    let decision = if observed_violations > policy.max_violations_in_window {
        GateDecision::Rollback
    } else {
        GateDecision::Proceed
    };
    HealthVerdict {
        decision,
        observed_violations,
        threshold: policy.max_violations_in_window,
        window_secs: policy.window_secs,
    }
}

/// Count how many violation events fall within `[now - window_secs, now]`.
/// Future events (`ts > now`) and stale events are excluded — same windowing
/// rule item-B recurrence uses. Pure; `now` is explicit.
///
/// `since` anchors the count to the canary stage's deploy time (Problem-2.2):
/// when `Some(anchor)`, any event with `ts < anchor` is excluded, so
/// violations that predate the stage deploy are never misattributed to the
/// canary. `None` imposes no lower bound (the original behavior — fully
/// backward-compatible: existing callers pass `None`).
pub fn violations_in_window(
    events: &[ViolationEvent],
    now: i64,
    window_secs: i64,
    since: Option<i64>,
) -> usize {
    events
        .iter()
        .filter(|ev| ev.ts <= now && now - ev.ts <= window_secs)
        .filter(|ev| since.is_none_or(|anchor| ev.ts >= anchor))
        .count()
}

/// Evaluate the canary health gate against the item-B violation registry.
///
/// Folds the raw events to a windowed violation count at `now` and compares
/// it against the policy threshold. `now` is explicit — there is NO
/// wall-clock read here, matching overwatch's item-B pattern, so the verdict
/// is reproducible under a seeded/injected time.
///
/// `since` (Problem-2.2) is the stage-deploy anchor: when `Some(anchor)`,
/// only violations at/after `anchor` are counted, so pre-deploy violations
/// are not blamed on the canary stage. `None` = no lower bound (unchanged
/// behavior for existing callers).
pub fn evaluate_health_gate(
    events: &[ViolationEvent],
    now: i64,
    policy: HealthGatePolicy,
    since: Option<i64>,
) -> HealthVerdict {
    let observed = violations_in_window(events, now, policy.window_secs, since);
    decide_from_count(observed, policy)
}

/// Adapter: evaluate the gate but count only *systemic* signatures (per the
/// item-B [`RecurrencePolicy`]) rather than raw events, so isolated one-off
/// noise doesn't trip a rollback. The gate threshold then applies to the
/// number of distinct systemic signatures observed in the window.
///
/// `since` (Problem-2.2) is the stage-deploy anchor: events with `ts < since`
/// are dropped BEFORE recurrence detection, so pre-deploy occurrences neither
/// count toward — nor establish — a systemic signature attributed to the
/// stage. `None` = no lower bound (unchanged behavior for existing callers).
pub fn evaluate_health_gate_systemic(
    events: &[ViolationEvent],
    now: i64,
    recurrence: RecurrencePolicy,
    policy: HealthGatePolicy,
    since: Option<i64>,
) -> HealthVerdict {
    // Anchor to the stage-deploy time first so pre-deploy events cannot form
    // (or add to) a systemic signature that would be mis-blamed on the stage.
    let anchored: Vec<ViolationEvent> = match since {
        Some(anchor) => events
            .iter()
            .filter(|ev| ev.ts >= anchor)
            .cloned()
            .collect(),
        None => events.to_vec(),
    };
    let systemic = detect_recurrence(&anchored, now, recurrence)
        .into_iter()
        .filter(|r| r.is_systemic)
        .count();
    decide_from_count(systemic, policy)
}

// ---------------------------------------------------------------------------
// Rollback plan
// ---------------------------------------------------------------------------

/// The current (canary) install state of one plugin, as read from the
/// registry before the canary stage was applied vs after.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginRollbackTarget {
    /// Plugin name.
    pub name: String,
    /// The version that was live BEFORE the canary stage (what a rollback
    /// restores to). `None` means the plugin was newly installed by the
    /// canary and a rollback would uninstall / leave it (see `is_new`).
    pub prior_version: Option<String>,
    /// The version the canary stage moved the plugin TO.
    pub canary_version: String,
    /// The cache version dir a rollback should re-point the registry at
    /// (i.e. the prior version's install path). `None` when `is_new`.
    pub restore_install_path: Option<String>,
    /// The install path the canary stage set (for reference / logging).
    pub canary_install_path: String,
    /// True when the plugin had no prior registry entry (newly introduced by
    /// the canary) — there is nothing to restore to, so a rollback must skip
    /// re-pointing and instead leave / remove it out-of-band.
    pub is_new: bool,
}

/// A full rollback plan for a canary stage: the per-plugin restore targets,
/// as DATA. Nothing here is executed — the shell script consumes this to
/// decide what to re-point, and only under the explicit opt-in flag.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RollbackPlan {
    /// The stage index this rollback plan applies to.
    pub stage_index: usize,
    /// Per-plugin restore targets, in the stage's plugin order.
    pub targets: Vec<PluginRollbackTarget>,
}

impl RollbackPlan {
    /// True iff every target in the stage is a fresh install with no prior
    /// version — a rollback then has nothing to re-point (only out-of-band
    /// removal would apply).
    pub fn all_new(&self) -> bool {
        !self.targets.is_empty() && self.targets.iter().all(|t| t.is_new)
    }
}

/// The registry state of one plugin captured just before a canary stage, used
/// to compute what a rollback restores. This is plain data the caller reads
/// from `installed_plugins.json` (name + prior version + prior install path);
/// this module never reads it itself.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PriorInstallState {
    /// Plugin name.
    pub name: String,
    /// The version live before the canary (None if the plugin was absent).
    pub prior_version: Option<String>,
    /// The install path live before the canary (None if the plugin was
    /// absent).
    pub prior_install_path: Option<String>,
}

/// One plugin's target version+path under the canary stage (what it moved to).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CanaryTarget {
    /// Plugin name.
    pub name: String,
    /// Version the canary stage rolled the plugin to.
    pub canary_version: String,
    /// Install path the canary stage set.
    pub canary_install_path: String,
}

/// Compute a [`RollbackPlan`] for one stage from (a) the prior install state
/// captured before the stage and (b) the canary targets the stage set.
///
/// Pure and deterministic: it joins the two by plugin name and emits, as
/// data, what a rollback would restore. It does NOT touch the filesystem or
/// registry. Prior states with no matching canary target are ignored (only
/// plugins the stage actually moved can be rolled back).
pub fn compute_rollback_plan(
    stage_index: usize,
    prior: &[PriorInstallState],
    canary_targets: &[CanaryTarget],
) -> RollbackPlan {
    let targets = canary_targets
        .iter()
        .map(|ct| {
            let prior_state = prior.iter().find(|p| p.name == ct.name);
            let prior_version = prior_state.and_then(|p| p.prior_version.clone());
            let restore_install_path = prior_state.and_then(|p| p.prior_install_path.clone());
            let is_new = prior_version.is_none();
            PluginRollbackTarget {
                name: ct.name.clone(),
                prior_version,
                canary_version: ct.canary_version.clone(),
                restore_install_path,
                canary_install_path: ct.canary_install_path.clone(),
                is_new,
            }
        })
        .collect();
    RollbackPlan {
        stage_index,
        targets,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::violation::ViolationSource;

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    // --- stage planning ----------------------------------------------------

    #[test]
    fn plan_by_size_splits_into_ordered_chunks() {
        let plugins = names(&["a", "b", "c", "d", "e"]);
        let plan = plan_stages_by_size(&plugins, 2);
        assert_eq!(plan.len(), 3);
        assert_eq!(plan.stages[0].plugins, names(&["a", "b"]));
        assert_eq!(plan.stages[1].plugins, names(&["c", "d"]));
        assert_eq!(plan.stages[2].plugins, names(&["e"]));
        // indices are 0-based and monotonic
        assert_eq!(plan.stages[0].index, 0);
        assert_eq!(plan.stages[2].index, 2);
    }

    #[test]
    fn plan_by_size_zero_degrades_to_one_per_stage() {
        let plugins = names(&["a", "b", "c"]);
        let plan = plan_stages_by_size(&plugins, 0);
        assert_eq!(plan.len(), 3);
        for (i, st) in plan.stages.iter().enumerate() {
            assert_eq!(st.plugins.len(), 1);
            assert_eq!(st.index, i);
        }
    }

    #[test]
    fn plan_by_size_empty_yields_empty_plan() {
        let plan = plan_stages_by_size(&[], 3);
        assert!(plan.is_empty());
        assert_eq!(plan.len(), 0);
    }

    #[test]
    fn plan_by_size_covers_every_plugin_exactly_once() {
        let plugins = names(&["a", "b", "c", "d", "e", "f", "g"]);
        let plan = plan_stages_by_size(&plugins, 3);
        let flat: Vec<String> = plan.stages.iter().flat_map(|s| s.plugins.clone()).collect();
        assert_eq!(flat, plugins);
    }

    #[test]
    fn plan_by_count_splits_evenly_with_remainder_up_front() {
        let plugins = names(&["a", "b", "c", "d", "e"]);
        let plan = plan_stages_by_count(&plugins, 2);
        assert_eq!(plan.len(), 2);
        // 5 into 2 → [3, 2]
        assert_eq!(plan.stages[0].plugins, names(&["a", "b", "c"]));
        assert_eq!(plan.stages[1].plugins, names(&["d", "e"]));
    }

    #[test]
    fn plan_by_count_more_stages_than_plugins_omits_empties() {
        let plugins = names(&["a", "b"]);
        let plan = plan_stages_by_count(&plugins, 5);
        assert_eq!(plan.len(), 2);
        assert_eq!(plan.stages[0].plugins, names(&["a"]));
        assert_eq!(plan.stages[1].plugins, names(&["b"]));
    }

    #[test]
    fn plan_by_count_zero_degrades_to_one_stage() {
        let plugins = names(&["a", "b", "c"]);
        let plan = plan_stages_by_count(&plugins, 0);
        assert_eq!(plan.len(), 1);
        assert_eq!(plan.stages[0].plugins, plugins);
    }

    #[test]
    fn plan_by_count_covers_every_plugin_exactly_once() {
        let plugins = names(&["a", "b", "c", "d", "e", "f", "g"]);
        let plan = plan_stages_by_count(&plugins, 3);
        let flat: Vec<String> = plan.stages.iter().flat_map(|s| s.plugins.clone()).collect();
        assert_eq!(flat, plugins);
    }

    #[test]
    fn stage_planning_is_deterministic() {
        let plugins = names(&["a", "b", "c", "d", "e"]);
        assert_eq!(
            plan_stages_by_size(&plugins, 2),
            plan_stages_by_size(&plugins, 2)
        );
        assert_eq!(
            plan_stages_by_count(&plugins, 3),
            plan_stages_by_count(&plugins, 3)
        );
    }

    // --- health gate -------------------------------------------------------

    fn viol(sig: &str, task: &str, session: &str, ts: i64) -> ViolationEvent {
        ViolationEvent {
            source: ViolationSource::Blastguard,
            signature: sig.to_string(),
            task_key: task.to_string(),
            session_id: session.to_string(),
            ts,
            detail: None,
        }
    }

    #[test]
    fn decide_from_count_proceeds_at_or_below_threshold() {
        let policy = HealthGatePolicy {
            max_violations_in_window: 2,
            window_secs: 900,
        };
        assert_eq!(decide_from_count(0, policy).decision, GateDecision::Proceed);
        assert_eq!(decide_from_count(2, policy).decision, GateDecision::Proceed);
    }

    #[test]
    fn decide_from_count_rolls_back_above_threshold() {
        let policy = HealthGatePolicy {
            max_violations_in_window: 2,
            window_secs: 900,
        };
        let v = decide_from_count(3, policy);
        assert_eq!(v.decision, GateDecision::Rollback);
        assert!(v.should_rollback());
        assert_eq!(v.observed_violations, 3);
        assert_eq!(v.threshold, 2);
    }

    #[test]
    fn violations_in_window_excludes_stale_and_future() {
        let events = vec![
            viol("s", "t1", "se1", 100),  // stale (out of window)
            viol("s", "t2", "se2", 950),  // in window
            viol("s", "t3", "se3", 1000), // in window (== now)
            viol("s", "t4", "se4", 5000), // future
        ];
        // now=1000, window=100 → in-window: ts in [900, 1000] → 950, 1000
        assert_eq!(violations_in_window(&events, 1000, 100, None), 2);
    }

    #[test]
    fn violations_in_window_since_excludes_pre_stage_and_counts_post_deploy() {
        // Problem-2.2: `since` anchors counting to the stage-deploy time.
        // Deploy happened at ts=940. A violation at ts=930 predates the deploy
        // (must be EXCLUDED); a violation at ts=940 (== since) and ts=980 are
        // at/after deploy (must be COUNTED). All three are within the raw
        // window, so only the `since` anchor distinguishes them.
        let events = vec![
            viol("s", "pre", "se-pre", 930),   // pre-stage: ts < since → excluded
            viol("s", "at", "se-at", 940),     // == since → counted (>= anchor)
            viol("s", "post", "se-post", 980), // post-deploy → counted
        ];
        let since = Some(940);
        // now=1000, window=900 → all three are within the raw window.
        assert_eq!(violations_in_window(&events, 1000, 900, None), 3);
        // With the deploy anchor, the pre-stage event is dropped → 2.
        assert_eq!(violations_in_window(&events, 1000, 900, since), 2);
    }

    #[test]
    fn evaluate_health_gate_since_anchor_excludes_pre_stage_violation() {
        // End-to-end through the gate: a pre-stage spike that WOULD trip the
        // gate without anchoring is correctly ignored once `since` anchors to
        // the deploy time, while a genuine post-deploy spike still rolls back.
        let policy = HealthGatePolicy {
            max_violations_in_window: 2,
            window_secs: 900,
        };
        // Three pre-stage violations (ts < 950) plus one post-deploy.
        let events = vec![
            viol("s", "t1", "se1", 910),
            viol("s", "t2", "se2", 920),
            viol("s", "t3", "se3", 930),
            viol("s", "t4", "se4", 960), // post-deploy
        ];
        // Without anchoring: 4 in window > 2 → Rollback (the misattribution bug).
        assert_eq!(
            evaluate_health_gate(&events, 1000, policy, None).decision,
            GateDecision::Rollback
        );
        // Anchored at the deploy time (950): only the ts=960 event counts → 1
        // ≤ 2 → Proceed. Pre-stage violations are no longer blamed on the stage.
        let v = evaluate_health_gate(&events, 1000, policy, Some(950));
        assert_eq!(v.decision, GateDecision::Proceed);
        assert_eq!(v.observed_violations, 1);

        // A real post-deploy spike (3 events at/after 950) still rolls back.
        let post_spike = vec![
            viol("s", "t1", "se1", 910), // pre-stage, excluded by anchor
            viol("s", "t5", "se5", 950),
            viol("s", "t6", "se6", 960),
            viol("s", "t7", "se7", 970),
        ];
        assert_eq!(
            evaluate_health_gate(&post_spike, 1000, policy, Some(950)).decision,
            GateDecision::Rollback
        );
    }

    #[test]
    fn evaluate_health_gate_proceeds_when_quiet() {
        let events = vec![viol("s", "t1", "se1", 950)];
        let policy = HealthGatePolicy {
            max_violations_in_window: 2,
            window_secs: 900,
        };
        let v = evaluate_health_gate(&events, 1000, policy, None);
        assert_eq!(v.decision, GateDecision::Proceed);
        assert_eq!(v.observed_violations, 1);
    }

    #[test]
    fn evaluate_health_gate_rolls_back_on_spike() {
        let events = vec![
            viol("s", "t1", "se1", 900),
            viol("s", "t2", "se2", 950),
            viol("s", "t3", "se3", 980),
        ];
        let policy = HealthGatePolicy {
            max_violations_in_window: 2,
            window_secs: 900,
        };
        let v = evaluate_health_gate(&events, 1000, policy, None);
        assert_eq!(v.decision, GateDecision::Rollback);
        assert_eq!(v.observed_violations, 3);
    }

    #[test]
    fn evaluate_health_gate_across_threshold_boundary() {
        // Exactly-at-threshold proceeds; one more rolls back.
        let mk = |n: usize| -> Vec<ViolationEvent> {
            (0..n)
                .map(|i| viol("s", &format!("t{i}"), "se", 950))
                .collect()
        };
        let policy = HealthGatePolicy {
            max_violations_in_window: 3,
            window_secs: 900,
        };
        assert_eq!(
            evaluate_health_gate(&mk(3), 1000, policy, None).decision,
            GateDecision::Proceed
        );
        assert_eq!(
            evaluate_health_gate(&mk(4), 1000, policy, None).decision,
            GateDecision::Rollback
        );
    }

    #[test]
    fn evaluate_health_gate_is_deterministic_under_injected_time() {
        let events = vec![viol("s", "t1", "se1", 900), viol("s", "t2", "se2", 950)];
        let policy = HealthGatePolicy::default();
        let a = evaluate_health_gate(&events, 1000, policy, None);
        let b = evaluate_health_gate(&events, 1000, policy, None);
        assert_eq!(a, b, "gate verdict must be pure/deterministic");
    }

    #[test]
    fn evaluate_health_gate_systemic_ignores_isolated_noise() {
        // 3 distinct isolated signatures (1 occurrence each) → 0 systemic.
        let events = vec![
            viol("sig-a", "t1", "se1", 950),
            viol("sig-b", "t2", "se2", 960),
            viol("sig-c", "t3", "se3", 970),
        ];
        let recurrence = RecurrencePolicy {
            threshold: 3,
            window_secs: 900,
        };
        let policy = HealthGatePolicy {
            max_violations_in_window: 0,
            window_secs: 900,
        };
        // No signature is systemic → observed systemic = 0 → Proceed even
        // though threshold is 0 (0 > 0 is false).
        let v = evaluate_health_gate_systemic(&events, 1000, recurrence, policy, None);
        assert_eq!(v.decision, GateDecision::Proceed);
        assert_eq!(v.observed_violations, 0);
    }

    #[test]
    fn evaluate_health_gate_systemic_trips_on_real_recurrence() {
        // Same signature across 3 distinct tasks → 1 systemic signature.
        let events = vec![
            viol("sig-a", "t1", "se1", 940),
            viol("sig-a", "t2", "se2", 950),
            viol("sig-a", "t3", "se3", 960),
        ];
        let recurrence = RecurrencePolicy {
            threshold: 3,
            window_secs: 900,
        };
        let policy = HealthGatePolicy {
            max_violations_in_window: 0,
            window_secs: 900,
        };
        let v = evaluate_health_gate_systemic(&events, 1000, recurrence, policy, None);
        assert_eq!(v.decision, GateDecision::Rollback);
        assert_eq!(v.observed_violations, 1);
    }

    // --- rollback plan -----------------------------------------------------

    fn prior(name: &str, ver: Option<&str>, path: Option<&str>) -> PriorInstallState {
        PriorInstallState {
            name: name.to_string(),
            prior_version: ver.map(str::to_string),
            prior_install_path: path.map(str::to_string),
        }
    }

    fn canary(name: &str, ver: &str, path: &str) -> CanaryTarget {
        CanaryTarget {
            name: name.to_string(),
            canary_version: ver.to_string(),
            canary_install_path: path.to_string(),
        }
    }

    #[test]
    fn compute_rollback_plan_restores_prior_version_and_path() {
        let prior_states = vec![prior(
            "condukt",
            Some("0.7.0"),
            Some("/cache/condukt/0.7.0"),
        )];
        let targets = vec![canary("condukt", "0.7.1", "/cache/condukt/0.7.1")];
        let plan = compute_rollback_plan(0, &prior_states, &targets);
        assert_eq!(plan.stage_index, 0);
        assert_eq!(plan.targets.len(), 1);
        let t = &plan.targets[0];
        assert_eq!(t.name, "condukt");
        assert_eq!(t.prior_version.as_deref(), Some("0.7.0"));
        assert_eq!(
            t.restore_install_path.as_deref(),
            Some("/cache/condukt/0.7.0")
        );
        assert_eq!(t.canary_version, "0.7.1");
        assert!(!t.is_new);
    }

    #[test]
    fn compute_rollback_plan_marks_new_plugin_as_new() {
        let prior_states: Vec<PriorInstallState> = vec![];
        let targets = vec![canary("brandnew", "0.1.0", "/cache/brandnew/0.1.0")];
        let plan = compute_rollback_plan(1, &prior_states, &targets);
        let t = &plan.targets[0];
        assert!(t.is_new);
        assert!(t.prior_version.is_none());
        assert!(t.restore_install_path.is_none());
        assert!(plan.all_new());
    }

    #[test]
    fn compute_rollback_plan_joins_only_moved_plugins() {
        // A prior state for a plugin NOT in this stage's canary targets is
        // ignored — only plugins the stage moved are rollback-able.
        let prior_states = vec![
            prior("condukt", Some("0.7.0"), Some("/cache/condukt/0.7.0")),
            prior("scout", Some("0.2.0"), Some("/cache/scout/0.2.0")),
        ];
        let targets = vec![canary("condukt", "0.7.1", "/cache/condukt/0.7.1")];
        let plan = compute_rollback_plan(0, &prior_states, &targets);
        assert_eq!(plan.targets.len(), 1);
        assert_eq!(plan.targets[0].name, "condukt");
    }

    #[test]
    fn compute_rollback_plan_is_deterministic() {
        let prior_states = vec![prior("a", Some("1.0.0"), Some("/c/a/1.0.0"))];
        let targets = vec![canary("a", "1.0.1", "/c/a/1.0.1")];
        let p1 = compute_rollback_plan(0, &prior_states, &targets);
        let p2 = compute_rollback_plan(0, &prior_states, &targets);
        assert_eq!(p1, p2);
    }

    #[test]
    fn all_new_false_when_any_target_has_prior() {
        let prior_states = vec![prior("a", Some("1.0.0"), Some("/c/a/1.0.0"))];
        let targets = vec![
            canary("a", "1.0.1", "/c/a/1.0.1"),
            canary("b", "0.1.0", "/c/b/0.1.0"),
        ];
        let plan = compute_rollback_plan(0, &prior_states, &targets);
        assert!(!plan.all_new());
    }
}
