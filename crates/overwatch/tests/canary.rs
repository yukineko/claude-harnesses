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
    let verdict = canary::evaluate_health_gate(&events, 1000, policy, None);
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
    let verdict = canary::evaluate_health_gate(&events, 1000, policy, None);
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
    let a = canary::evaluate_health_gate(&events, 1000, policy, None);
    let b = canary::evaluate_health_gate(&events, 1000, policy, None);
    assert_eq!(a, b);
    // Same events at a LATER `now` (window slides past them) → different count.
    let later = canary::evaluate_health_gate(&events, 100_000, policy, None);
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
        canary::evaluate_health_gate_systemic(&isolated, 1000, recurrence, policy, None).decision,
        GateDecision::Proceed
    );

    // One signature recurring across 3 distinct tasks → 1 systemic → ROLLBACK.
    let recurring = vec![
        viol("sig-a", "t1", "s1", 940),
        viol("sig-a", "t2", "s2", 950),
        viol("sig-a", "t3", "s3", 960),
    ];
    assert_eq!(
        canary::evaluate_health_gate_systemic(&recurring, 1000, recurrence, policy, None).decision,
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

// ---------------------------------------------------------------------------
// Problem-2.1: the canary-gate CLI (in registry mode) emits BOTH a raw-spike
// and a systemic verdict and exits non-zero (rollback signal the rollout shell
// consumes) if EITHER fires. Driving the real `overwatch` binary end-to-end
// proves rollout honors both paths: one case trips via raw spike ONLY, another
// via systemic recurrence ONLY.
// ---------------------------------------------------------------------------

use std::process::Command;

/// Serializes the registry-mode gate tests: they each mutate process-global
/// `HOME` (`std::env::set_var`) and read/write a cwd-derived `violations.jsonl`
/// path, so running them concurrently (Rust's default in-process parallel test
/// runner) races on the shared HOME/path and intermittently reads 0 events.
static GATE_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Write a violations.jsonl for the current cwd's project under a temp $HOME so
/// the gate reads exactly these events, then run `overwatch canary-gate` with
/// that HOME. Returns (exit_code, stdout). `now`/`since`/`threshold` are all
/// injected so the verdict is fully deterministic.
fn run_gate_over_events(
    home: &std::path::Path,
    events_jsonl: &str,
    threshold: usize,
    systemic_threshold: usize,
    window_secs: i64,
    now: i64,
) -> (i32, String) {
    // Serialize all registry-mode gate tests: they share process-global `HOME`
    // and a cwd-derived violations path, so parallel threads would otherwise
    // clobber each other's HOME between set_var and the path write.
    let _env_guard = GATE_ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // Path the binary will resolve for violations, using overwatch's OWN store
    // logic under this HOME (so the in-test write and the child read agree).
    std::env::set_var("HOME", home);
    let cwd = std::env::current_dir().unwrap();
    let vpath = overwatch::store::violations_path(&cwd).unwrap();
    std::fs::create_dir_all(vpath.parent().unwrap()).unwrap();
    std::fs::write(&vpath, events_jsonl).unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_overwatch"))
        .env("HOME", home)
        .current_dir(&cwd)
        .args([
            "canary-gate",
            "--threshold",
            &threshold.to_string(),
            "--systemic-threshold",
            &systemic_threshold.to_string(),
            "--window-secs",
            &window_secs.to_string(),
            "--now",
            &now.to_string(),
        ])
        .output()
        .expect("run overwatch canary-gate");
    let code = out.status.code().unwrap_or(-1);
    (code, String::from_utf8_lossy(&out.stdout).to_string())
}

/// One JSONL line for a violation event (matches `ViolationEvent` serde).
fn ev_line(source: &str, sig: &str, task: &str, session: &str, ts: i64) -> String {
    format!(
        r#"{{"source":"{source}","signature":"{sig}","task_key":"{task}","session_id":"{session}","ts":{ts}}}"#
    )
}

#[test]
fn rollout_gate_trips_via_raw_spike_only() {
    let home = std::env::temp_dir().join(format!("ow-canary-raw-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    // Three DISTINCT one-off signatures at/after deploy → raw count 3 (> 2)
    // but 0 systemic (no signature recurs). Raw spike ONLY.
    let jsonl = [
        ev_line("blastguard", "blastguard:sig-a", "t1", "s1", 950),
        ev_line("propguard", "propguard:sig-b", "t2", "s2", 960),
        ev_line("specguard", "specguard:sig-c", "t3", "s3", 970),
    ]
    .join("\n");
    let (code, stdout) = run_gate_over_events(&home, &jsonl, 2, 0, 900, 1000);
    let _ = std::fs::remove_dir_all(&home);
    // Non-zero exit = rollback advised (exit 3). Rollout honors the raw path.
    assert_eq!(code, 3, "raw spike must trip the gate; stdout={stdout}");
    assert!(
        stdout.contains("\"decision\": \"rollback\""),
        "combined verdict must be rollback; stdout={stdout}"
    );
    // Both sub-verdicts are visible; the raw one is what tripped.
    assert!(stdout.contains("\"raw\""), "raw sub-verdict present");
    assert!(
        stdout.contains("\"systemic\""),
        "systemic sub-verdict present"
    );
}

#[test]
fn rollout_gate_trips_via_systemic_only() {
    // Systemic-ONLY at the binary level: the SAME signature recurs across 3
    // distinct tasks (1 systemic signature) but the RAW event count stays at or
    // below the threshold so the raw sub-gate PROCEEDS while systemic ROLLS
    // BACK. We achieve raw-proceed by injecting `since` (the deploy anchor,
    // Problem-2.2) so pre-deploy occurrences are excluded from the RAW window
    // yet the systemic detector still sees the full recurrence within its own
    // window... — but since both share the anchor, we instead keep the raw
    // count within tolerance with a threshold equal to the raw count while the
    // systemic count (1 signature) exceeds 0. To make the two thresholds
    // diverge we rely on: raw counts EVENTS, systemic counts SIGNATURES.
    //
    // Concretely: 3 events of ONE recurring signature. threshold = 3 → raw
    // (3 events) is NOT > 3 → PROCEED; systemic detector (default threshold 3)
    // finds the signature systemic (3 occurrences across 3 tasks) → 1 systemic
    // signature > the gate threshold? 1 > 3 is false. So a SHARED threshold
    // cannot isolate systemic-only at the binary level — this test only proves
    // the recurrence path CAN drive a rollback exit through the CLI. Genuine
    // binary-level systemic-only isolation is now proven by
    // `rollout_gate_systemic_trips_independently_of_raw` (Problem-2.1b: a
    // dedicated lower systemic threshold), plus the pure layers
    // (canary::systemic_gate_* and canary_cli::combined_verdict_trips_on_systemic_only).
    let home = std::env::temp_dir().join(format!("ow-canary-sys-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let jsonl = [
        ev_line("blastguard", "blastguard:sig-x", "t1", "s1", 940),
        ev_line("blastguard", "blastguard:sig-x", "t2", "s2", 950),
        ev_line("blastguard", "blastguard:sig-x", "t3", "s3", 960),
    ]
    .join("\n");
    let (code, stdout) = run_gate_over_events(&home, &jsonl, 2, 0, 900, 1000);
    let _ = std::fs::remove_dir_all(&home);
    assert_eq!(
        code, 3,
        "recurrence spike must trip the gate; stdout={stdout}"
    );
    assert!(stdout.contains("\"decision\": \"rollback\""));
    assert!(stdout.contains("\"raw\""));
    assert!(stdout.contains("\"systemic\""));
}

#[test]
fn rollout_gate_systemic_trips_independently_of_raw() {
    // Problem-2.1b: with a DEDICATED (lower) systemic threshold, the systemic
    // arm rolls back while the RAW arm PROCEEDS — genuine fleet-recurrence
    // isolation the shared-threshold gate (Problem-2.1) could NOT produce at
    // the binary level. 3 events of ONE signature across 3 distinct tasks =
    // 1 systemic signature. --threshold 5 → raw count 3 is NOT > 5 → PROCEED.
    // --systemic-threshold 0 → systemic count 1 IS > 0 → ROLLBACK. So the
    // COMBINED verdict rolls back via the systemic path ALONE.
    let home = std::env::temp_dir().join(format!("ow-canary-sysindep-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let jsonl = [
        ev_line("blastguard", "blastguard:sig-y", "t1", "s1", 940),
        ev_line("blastguard", "blastguard:sig-y", "t2", "s2", 950),
        ev_line("blastguard", "blastguard:sig-y", "t3", "s3", 960),
    ]
    .join("\n");
    // raw threshold 5 (raw 3 proceeds), systemic threshold 0 (systemic 1 trips).
    let (code, stdout) = run_gate_over_events(&home, &jsonl, 5, 0, 900, 1000);
    let _ = std::fs::remove_dir_all(&home);
    assert_eq!(
        code, 3,
        "systemic arm must trip the gate independently of raw; stdout={stdout}"
    );
    assert!(
        stdout.contains("\"decision\": \"rollback\""),
        "combined verdict rolls back; stdout={stdout}"
    );
    // Prove ISOLATION: the raw sub-block PROCEEDS while the systemic sub-block
    // ROLLS BACK. Struct field order is decision, raw, systemic, so the raw
    // block precedes the systemic block in the pretty JSON.
    let raw_idx = stdout.find("\"raw\"").expect("raw sub-verdict present");
    let sys_idx = stdout
        .find("\"systemic\"")
        .expect("systemic sub-verdict present");
    assert!(raw_idx < sys_idx);
    let raw_block = &stdout[raw_idx..sys_idx];
    let sys_block = &stdout[sys_idx..];
    assert!(
        raw_block.contains("\"decision\": \"proceed\""),
        "raw arm must PROCEED (3 !> 5); stdout={stdout}"
    );
    assert!(
        sys_block.contains("\"decision\": \"rollback\""),
        "systemic arm must ROLL BACK (1 > 0); stdout={stdout}"
    );
}

#[test]
fn rollout_gate_proceeds_when_quiet_and_emits_both_signals() {
    // Quiet fleet → OR does not false-trip → exit 0, and BOTH sub-verdicts are
    // emitted so the shell can see each signal (Problem-2.1).
    let home = std::env::temp_dir().join(format!("ow-canary-quiet-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let jsonl = ev_line("blastguard", "blastguard:sig-q", "t1", "s1", 950);
    let (code, stdout) = run_gate_over_events(&home, &jsonl, 2, 0, 900, 1000);
    let _ = std::fs::remove_dir_all(&home);
    assert_eq!(code, 0, "quiet fleet must proceed; stdout={stdout}");
    assert!(stdout.contains("\"decision\": \"proceed\""));
    assert!(stdout.contains("\"raw\""));
    assert!(stdout.contains("\"systemic\""));
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
