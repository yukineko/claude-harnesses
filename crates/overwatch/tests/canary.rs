//! Integration tests for the opt-in canary staged-rollout core: exercising the
//! public `overwatch::canary` API end-to-end (stage planning, health gating
//! against the item-B violation registry, and rollback-plan computation) the
//! way `scripts/rollout-plugins.sh --canary` would drive it.
//!
//! Everything here is pure/deterministic — no filesystem, no clock, no real
//! rollout is touched. `now` is injected explicitly.
use overwatch::canary::{self, CanaryTarget, GateDecision, HealthGatePolicy, PriorInstallState};
use overwatch::violation::{RecurrencePolicy, ViolationEvent, ViolationSource};

fn names(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

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
fn full_canary_flow_proceeds_when_fleet_is_quiet() {
    // Plan: 5 plugins, 2 per stage → 3 stages.
    let plugins = names(&["condukt", "scout", "overwatch", "specguard", "ship"]);
    let plan = canary::plan_stages_by_size(&plugins, 2);
    assert_eq!(plan.len(), 3);

    // After rolling out stage 0, the fleet has a single stray violation —
    // below the tolerance of 2 → PROCEED.
    let events = vec![viol("blastguard:rm-rf", "t1", "s1", 950)];
    let policy = HealthGatePolicy {
        max_violations_in_window: 2,
        window_secs: 900,
    };
    let verdict = canary::evaluate_health_gate(&events, 1000, policy);
    assert_eq!(verdict.decision, GateDecision::Proceed);
    assert!(!verdict.should_rollback());
}

#[test]
fn full_canary_flow_rolls_back_on_violation_spike() {
    let plugins = names(&["condukt", "scout", "overwatch"]);
    let plan = canary::plan_stages_by_count(&plugins, 3);
    assert_eq!(plan.len(), 3);
    assert_eq!(plan.stages[0].plugins, names(&["condukt"]));

    // Stage 0 rolled out condukt; a spike of 3 violations appears within the
    // window → the gate advises ROLLBACK of stage 0.
    let events = vec![
        viol("blastguard:rm-rf", "t1", "s1", 910),
        viol("propguard:prop-3", "t2", "s2", 950),
        viol("specguard:drift:x", "t3", "s3", 980),
    ];
    let policy = HealthGatePolicy {
        max_violations_in_window: 2,
        window_secs: 900,
    };
    let verdict = canary::evaluate_health_gate(&events, 1000, policy);
    assert_eq!(verdict.decision, GateDecision::Rollback);

    // The rollback plan restores condukt to its prior version/path.
    let prior = vec![PriorInstallState {
        name: "condukt".to_string(),
        prior_version: Some("0.7.0".to_string()),
        prior_install_path: Some("/cache/yukineko/condukt/0.7.0".to_string()),
    }];
    let targets = vec![CanaryTarget {
        name: "condukt".to_string(),
        canary_version: "0.7.1".to_string(),
        canary_install_path: "/cache/yukineko/condukt/0.7.1".to_string(),
    }];
    let rb = canary::compute_rollback_plan(plan.stages[0].index, &prior, &targets);
    assert_eq!(rb.stage_index, 0);
    assert_eq!(rb.targets.len(), 1);
    assert_eq!(rb.targets[0].prior_version.as_deref(), Some("0.7.0"));
    assert_eq!(
        rb.targets[0].restore_install_path.as_deref(),
        Some("/cache/yukineko/condukt/0.7.0")
    );
    assert!(!rb.targets[0].is_new);
}

#[test]
fn health_gate_is_deterministic_under_injected_time() {
    let events = vec![
        viol("blastguard:rm-rf", "t1", "s1", 910),
        viol("propguard:prop-3", "t2", "s2", 950),
    ];
    let policy = HealthGatePolicy::default();
    let a = canary::evaluate_health_gate(&events, 1000, policy);
    let b = canary::evaluate_health_gate(&events, 1000, policy);
    assert_eq!(a, b);
    // Same events at a LATER `now` (window slides past them) → different count.
    let later = canary::evaluate_health_gate(&events, 100_000, policy);
    assert_eq!(later.observed_violations, 0);
    assert_eq!(later.decision, GateDecision::Proceed);
}

#[test]
fn systemic_gate_ignores_isolated_noise_but_trips_on_recurrence() {
    let recurrence = RecurrencePolicy {
        threshold: 3,
        window_secs: 900,
    };
    let policy = HealthGatePolicy {
        max_violations_in_window: 0,
        window_secs: 900,
    };

    // Three distinct isolated signatures → 0 systemic → PROCEED.
    let isolated = vec![
        viol("sig-a", "t1", "s1", 950),
        viol("sig-b", "t2", "s2", 960),
        viol("sig-c", "t3", "s3", 970),
    ];
    assert_eq!(
        canary::evaluate_health_gate_systemic(&isolated, 1000, recurrence, policy).decision,
        GateDecision::Proceed
    );

    // One signature recurring across 3 distinct tasks → 1 systemic → ROLLBACK.
    let recurring = vec![
        viol("sig-a", "t1", "s1", 940),
        viol("sig-a", "t2", "s2", 950),
        viol("sig-a", "t3", "s3", 960),
    ];
    assert_eq!(
        canary::evaluate_health_gate_systemic(&recurring, 1000, recurrence, policy).decision,
        GateDecision::Rollback
    );
}

#[test]
fn rollback_plan_flags_newly_introduced_plugins() {
    // A plugin with no prior registry entry cannot be rolled back to a prior
    // version — the plan marks it `is_new` so the shell skips re-pointing it.
    let prior: Vec<PriorInstallState> = vec![];
    let targets = vec![CanaryTarget {
        name: "brandnew".to_string(),
        canary_version: "0.1.0".to_string(),
        canary_install_path: "/cache/yukineko/brandnew/0.1.0".to_string(),
    }];
    let rb = canary::compute_rollback_plan(0, &prior, &targets);
    assert!(rb.all_new());
    assert!(rb.targets[0].is_new);
    assert!(rb.targets[0].restore_install_path.is_none());
}

#[test]
fn stage_plan_covers_all_plugins_in_order() {
    let plugins = names(&["a", "b", "c", "d", "e", "f", "g"]);
    for &size in &[1usize, 2, 3, 7, 100] {
        let plan = canary::plan_stages_by_size(&plugins, size);
        let flat: Vec<String> = plan.stages.iter().flat_map(|s| s.plugins.clone()).collect();
        assert_eq!(flat, plugins, "stage_size={size} must cover all in order");
    }
    for &count in &[1usize, 2, 3, 7, 100] {
        let plan = canary::plan_stages_by_count(&plugins, count);
        let flat: Vec<String> = plan.stages.iter().flat_map(|s| s.plugins.clone()).collect();
        assert_eq!(flat, plugins, "stage_count={count} must cover all in order");
    }
}
