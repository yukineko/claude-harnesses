//! CIRCUIT-BREAKER gate: deterministic "keep looping vs trip" decision core.
//!
//! An autonomous run loop can burn cost or spin forever when something has gone
//! wrong: a task fails over and over, the budget blows past its cap, or the run
//! stops making progress and idles. This module is the deterministic core
//! consulted by `condukt circuit check` on each loop iteration: given a handful
//! of already-gathered signals (a consecutive-failure streak, a
//! budget-over-cap flag, and an idle duration) it decides whether to `Continue`
//! the loop or `Trip` the breaker with a stable reason.
//!
//! Purity guarantee (mirrors [`crate::run_policy::decide_run_policy`] and
//! [`crate::policy::decide`]): no filesystem, no `std::time`, no env, no LLM.
//! The caller gathers the signals; this function is a total, deterministic
//! function of its arguments and never panics. The caps are opt-out: a
//! `streak_cap` of 0 disables the streak condition, and an `idle_ttl_secs` of 0
//! disables the stall condition.

use crate::config::Config;
use crate::state::{self, RunState, Status};
use std::path::Path;

/// Why the circuit breaker tripped. Stable lowercase slugs (via [`as_str`]) are
/// journaled so downstream tooling can key off the reason.
///
/// [`as_str`]: CircuitReason::as_str
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CircuitReason {
    /// The consecutive-failure streak reached (or exceeded) its cap.
    FailureStreak,
    /// The run's budget went over its cap — the hardest stop (cost).
    BudgetOverCap,
    /// The run idled at least as long as its time-to-live without progress.
    Stall,
}

impl CircuitReason {
    /// A stable lowercase slug for journaling (`"failure_streak"` /
    /// `"budget_over_cap"` / `"stall"`).
    pub fn as_str(&self) -> &'static str {
        match self {
            CircuitReason::FailureStreak => "failure_streak",
            CircuitReason::BudgetOverCap => "budget_over_cap",
            CircuitReason::Stall => "stall",
        }
    }
}

/// The two-state verdict emitted by [`decide_circuit`]: keep looping, or trip
/// the breaker with a reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CircuitVerdict {
    /// No stop condition holds — keep looping.
    Continue,
    /// A stop condition holds — trip the breaker for the carried reason.
    Trip(CircuitReason),
}

/// Decide whether to keep looping or trip the circuit breaker.
///
/// Pure and deterministic: no LLM, no network, no filesystem, no clock. Never
/// panics. Conditions are checked in a fixed precedence (first match wins):
///
/// 1. `budget_over_cap` → `Trip(BudgetOverCap)` (cost is the hardest stop).
/// 2. else `streak_cap > 0 && streak >= streak_cap` → `Trip(FailureStreak)`.
/// 3. else `idle_ttl_secs > 0 && idle_secs >= idle_ttl_secs` → `Trip(Stall)`.
/// 4. otherwise → `Continue`.
///
/// A `streak_cap` of 0 disables the streak condition; an `idle_ttl_secs` of 0
/// disables the stall condition. These opt-outs keep the function total.
pub fn decide_circuit(
    streak: u32,
    streak_cap: u32,
    budget_over_cap: bool,
    idle_secs: i64,
    idle_ttl_secs: i64,
) -> CircuitVerdict {
    if budget_over_cap {
        CircuitVerdict::Trip(CircuitReason::BudgetOverCap)
    } else if streak_cap > 0 && streak >= streak_cap {
        CircuitVerdict::Trip(CircuitReason::FailureStreak)
    } else if idle_ttl_secs > 0 && idle_secs >= idle_ttl_secs {
        CircuitVerdict::Trip(CircuitReason::Stall)
    } else {
        CircuitVerdict::Continue
    }
}

// ── signal gathering (the CLI layer; clock/FS live HERE, not in the core) ────

/// The number of TRAILING consecutive `Failed` tasks when the run's tasks are
/// ordered by `updated_at` ascending (i.e. the most-recent unbroken run of
/// failures). Pure and total: a task with no `updated_at` sorts as `0` (oldest),
/// and an empty/failure-free tail yields `0`. Never panics.
pub(crate) fn trailing_failure_streak(run: &RunState) -> u32 {
    let mut idx: Vec<usize> = (0..run.tasks.len()).collect();
    // Stable sort by updated_at asc; None sorts oldest. Stability keeps the
    // relative order of equal timestamps deterministic.
    idx.sort_by_key(|&i| run.tasks[i].updated_at.unwrap_or(0));
    let mut streak: u32 = 0;
    for &i in idx.iter().rev() {
        if run.tasks[i].status == Status::Failed {
            streak = streak.saturating_add(1);
        } else {
            break;
        }
    }
    streak
}

/// The maximum `updated_at` across the run's tasks — the moment the run last
/// made progress. `None` when no task carries a timestamp (legacy data).
pub(crate) fn max_updated_at(run: &RunState) -> Option<i64> {
    run.tasks.iter().filter_map(|t| t.updated_at).max()
}

/// Handler for `condukt circuit check --run RID`. Gathers the three signals
/// FAIL-SOFT (any gathering error degrades to the non-tripping value and never
/// panics), runs the pure [`decide_circuit`] core, prints the verdict + signals
/// as JSON on stdout, journals the same record fail-soft, and returns the
/// process exit code (`0` = continue, `1` = trip) so the loops can do
/// `if ! condukt circuit check --run RID; then stop; fi`.
pub fn run_circuit_check(
    cfg: &Config,
    cwd: &Path,
    run_id: &str,
    streak_cap: u32,
    idle_ttl_secs: i64,
    budget_cap_usd: Option<f64>,
) -> i32 {
    // 1. failure-streak — load the run fail-soft; an unloadable run → streak 0.
    let run = RunState::load(cfg, cwd, run_id).ok();
    let streak = run.as_ref().map(trailing_failure_streak).unwrap_or(0);

    // 2. budget_over_cap — LEAST-COUPLING source: read budgetguard's on-disk
    //    day-usage ledger through the SHARED `harness_core::ledger::Ledger`
    //    type. condukt already links harness-core, so this adds NO new crate
    //    coupling — we do NOT add a `budgetguard = { path }` dependency (and
    //    budgetguard is a binary-only crate with no lib target to depend on
    //    anyway), and we do NOT parse budgetguard's private config. The cap is
    //    injected via `--budget-cap-usd`; an absent/<=0 cap, or an unavailable/
    //    absent ledger, fail-softs to `false` (non-trip). No network calls.
    let budget_over_cap = match budget_cap_usd {
        Some(cap) if cap > 0.0 => {
            let today = chrono::Local::now().format("%Y-%m-%d").to_string();
            let day_usd =
                harness_core::ledger::Ledger::load(&harness_core::ledger::default_state_dir())
                    .day_total(&today);
            day_usd >= cap
        }
        _ => false,
    };

    // 3. idle_secs — now minus the run's most-recent progress. Clock use is
    //    allowed HERE (CLI layer); only the pure core stays clock-free. No
    //    timestamp available → 0 (non-trip). Clamped at 0 (clock skew fail-soft).
    let idle_secs = run
        .as_ref()
        .and_then(max_updated_at)
        .map(|ts| (state::now_secs() - ts).max(0))
        .unwrap_or(0);

    let verdict = decide_circuit(
        streak,
        streak_cap,
        budget_over_cap,
        idle_secs,
        idle_ttl_secs,
    );
    let (verdict_str, reason): (&str, Option<String>) = match &verdict {
        CircuitVerdict::Continue => ("continue", None),
        CircuitVerdict::Trip(r) => ("trip", Some(r.as_str().to_string())),
    };

    // Observable stdout JSON (verdict + reason slug + every gathered signal).
    let out = serde_json::json!({
        "verdict": verdict_str,
        "reason": reason,
        "streak": streak,
        "streak_cap": streak_cap,
        "budget_over_cap": budget_over_cap,
        "idle_secs": idle_secs,
        "idle_ttl_secs": idle_ttl_secs,
    });
    println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());

    // Journal the same record to the append-only JSONL trail — FAIL-SOFT: a
    // journaling failure must never change the exit code (mirrors gatelog).
    let record = crate::gatelog::CircuitRecord {
        verdict: verdict_str.to_string(),
        reason,
        streak,
        streak_cap,
        budget_over_cap,
        idle_secs,
        idle_ttl_secs,
        recorded_at: state::now_secs(),
    };
    crate::gatelog::append_circuit(&state::project_state_dir(cfg, cwd), run_id, &record);

    match verdict {
        CircuitVerdict::Continue => 0,
        CircuitVerdict::Trip(_) => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── each trip reason reachable ─────────────────────────────────────────

    #[test]
    fn budget_over_cap_trips() {
        let v = decide_circuit(0, 5, true, 0, 60);
        assert_eq!(v, CircuitVerdict::Trip(CircuitReason::BudgetOverCap));
    }

    #[test]
    fn failure_streak_trips() {
        let v = decide_circuit(5, 5, false, 0, 60);
        assert_eq!(v, CircuitVerdict::Trip(CircuitReason::FailureStreak));
    }

    #[test]
    fn stall_trips() {
        let v = decide_circuit(0, 5, false, 60, 60);
        assert_eq!(v, CircuitVerdict::Trip(CircuitReason::Stall));
        // as_str slug is stable/journalable.
        if let CircuitVerdict::Trip(reason) = v {
            assert_eq!(reason.as_str(), "stall");
        } else {
            panic!("expected a Trip verdict");
        }
    }

    // ── the no-trip Continue case ──────────────────────────────────────────

    #[test]
    fn no_condition_holds_continues() {
        let v = decide_circuit(2, 5, false, 30, 60);
        assert_eq!(v, CircuitVerdict::Continue);
    }

    // ── boundary equality ──────────────────────────────────────────────────

    #[test]
    fn streak_equal_to_cap_trips() {
        assert_eq!(
            decide_circuit(5, 5, false, 0, 0),
            CircuitVerdict::Trip(CircuitReason::FailureStreak)
        );
    }

    #[test]
    fn streak_one_below_cap_does_not_trip() {
        assert_eq!(decide_circuit(4, 5, false, 0, 0), CircuitVerdict::Continue);
    }

    #[test]
    fn idle_equal_to_ttl_trips() {
        assert_eq!(
            decide_circuit(0, 0, false, 60, 60),
            CircuitVerdict::Trip(CircuitReason::Stall)
        );
    }

    #[test]
    fn idle_one_below_ttl_does_not_trip() {
        assert_eq!(
            decide_circuit(0, 0, false, 59, 60),
            CircuitVerdict::Continue
        );
    }

    // ── disabling semantics (cap of 0 opts the axis out) ───────────────────

    #[test]
    fn streak_cap_zero_never_trips_on_streak() {
        // A huge streak but streak_cap == 0 disables the condition.
        assert_eq!(
            decide_circuit(u32::MAX, 0, false, 0, 0),
            CircuitVerdict::Continue
        );
    }

    #[test]
    fn idle_ttl_zero_never_trips_on_stall() {
        // A huge idle but idle_ttl_secs == 0 disables the condition.
        assert_eq!(
            decide_circuit(0, 0, false, i64::MAX, 0),
            CircuitVerdict::Continue
        );
    }

    // ── precedence: budget beats streak beats stall ────────────────────────

    #[test]
    fn budget_beats_streak_and_stall() {
        // All three conditions hold; budget wins.
        let v = decide_circuit(10, 5, true, 100, 60);
        assert_eq!(v, CircuitVerdict::Trip(CircuitReason::BudgetOverCap));
        assert_eq!(
            match &v {
                CircuitVerdict::Trip(r) => r.as_str(),
                _ => "continue",
            },
            "budget_over_cap"
        );
    }

    #[test]
    fn streak_beats_stall() {
        // Both streak and stall hold (no budget); streak wins.
        let v = decide_circuit(10, 5, false, 100, 60);
        assert_eq!(v, CircuitVerdict::Trip(CircuitReason::FailureStreak));
    }

    // ── determinism ────────────────────────────────────────────────────────

    #[test]
    fn decide_circuit_is_deterministic() {
        let v1 = decide_circuit(10, 5, false, 100, 60);
        let v2 = decide_circuit(10, 5, false, 100, 60);
        assert_eq!(v1, v2);
    }

    // ── every reason slug is stable ────────────────────────────────────────

    #[test]
    fn reason_slugs_are_stable() {
        assert_eq!(CircuitReason::FailureStreak.as_str(), "failure_streak");
        assert_eq!(CircuitReason::BudgetOverCap.as_str(), "budget_over_cap");
        assert_eq!(CircuitReason::Stall.as_str(), "stall");
    }

    // ── signal-gathering helpers + the CLI handler (wired path) ────────────

    use crate::config::Config;
    use crate::state::{RunState, TaskState};

    fn test_cfg(tmp: &Path) -> Config {
        Config {
            worktree_base: tmp.join("worktrees"),
            default_branch: "main".to_string(),
            shared_globs: Vec::new(),
            max_parallel: 4,
            state_dir: tmp.to_path_buf(),
            test_command: None,
            stuck_ttl_secs: 1800,
            build_command: None,
            deploy_command: None,
            loop_max_iters: 10,
            autonomous: false,
            consensus_enabled: false,
            consensus_samples: crate::consensus::DEFAULT_SAMPLES,
            consensus_threshold: crate::consensus::DEFAULT_THRESHOLD,
            adversarial_enabled: false,
            adversarial_size: crate::adversarial::DEFAULT_PANEL,
            adversarial_min_voters: crate::adversarial::DEFAULT_MIN_VOTERS,
            adversarial_block_ratio: crate::adversarial::DEFAULT_BLOCK_RATIO,
            single_worktree: false,
            worker_sandbox_enabled: false,
            worker_sandbox_image: None,
            worker_sandbox_memory: None,
            worker_sandbox_cpus: None,
            worker_sandbox_pids_limit: None,
        }
    }

    fn task(id: &str, status: Status, updated_at: Option<i64>) -> TaskState {
        TaskState {
            id: id.to_string(),
            status,
            worktree: None,
            branch: None,
            branch_sha: None,
            updated_at,
            model: None,
            cost_usd: None,
            fp_oracle_valid: None,
            findings: None,
            hashkey: None,
            claimed_at: None,
            started_at: None,
            agent_id: None,
        }
    }

    fn run_with(run_id: &str, tasks: Vec<TaskState>) -> RunState {
        RunState {
            run_id: run_id.to_string(),
            goal: "g".to_string(),
            tasks,
            paused: false,
            terminal_label: None,
            recorded_at: None,
        }
    }

    #[test]
    fn trailing_streak_counts_only_the_most_recent_run_of_failures() {
        // ordered by updated_at asc: failed(1), verified(2), failed(3), failed(4)
        let run = run_with(
            "r",
            vec![
                task("a", Status::Failed, Some(1)),
                task("d", Status::Failed, Some(4)),
                task("b", Status::Verified, Some(2)),
                task("c", Status::Failed, Some(3)),
            ],
        );
        // trailing run is c(3), d(4) → 2 (the earlier failed a(1) is broken by b).
        assert_eq!(trailing_failure_streak(&run), 2);
    }

    #[test]
    fn max_updated_at_picks_the_latest_progress() {
        let run = run_with(
            "r",
            vec![
                task("a", Status::Failed, Some(10)),
                task("b", Status::Done, Some(42)),
                task("c", Status::Pending, None),
            ],
        );
        assert_eq!(max_updated_at(&run), Some(42));
        let none = run_with("r", vec![task("a", Status::Pending, None)]);
        assert_eq!(max_updated_at(&none), None);
    }

    #[test]
    fn failure_streak_over_cap_trips_with_nonzero_exit() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path();
        let cfg = test_cfg(cwd);
        // Three trailing failures with a fresh timestamp (so the stall axis does
        // not fire); streak_cap default 3 → trips on failure_streak.
        let now = state::now_secs();
        let run = run_with(
            "trip-run",
            vec![
                task("a", Status::Failed, Some(now - 3)),
                task("b", Status::Failed, Some(now - 2)),
                task("c", Status::Failed, Some(now - 1)),
            ],
        );
        run.save(&cfg, cwd).unwrap();
        let code = run_circuit_check(&cfg, cwd, "trip-run", 3, 1800, None);
        assert_eq!(code, 1, "beyond-cap failure streak must trip (exit 1)");
        // and it journaled a trip record with the failure_streak reason.
        let recs =
            crate::gatelog::load_circuit_records(&state::project_state_dir(&cfg, cwd), "trip-run");
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].verdict, "trip");
        assert_eq!(recs[0].reason.as_deref(), Some("failure_streak"));
        assert_eq!(recs[0].streak, 3);
    }

    #[test]
    fn healthy_run_continues_with_zero_exit() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path();
        let cfg = test_cfg(cwd);
        let now = state::now_secs();
        let run = run_with(
            "healthy",
            vec![
                task("a", Status::Verified, Some(now - 5)),
                task("b", Status::Running, Some(now)),
            ],
        );
        run.save(&cfg, cwd).unwrap();
        // No cap on budget (None → non-trip); fresh timestamps → no stall.
        let code = run_circuit_check(&cfg, cwd, "healthy", 3, 1800, None);
        assert_eq!(code, 0, "a healthy run must continue (exit 0)");
    }

    #[test]
    fn missing_run_fails_soft_to_continue_not_panic() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path();
        let cfg = test_cfg(cwd);
        // No run saved: streak 0, idle 0, budget off → Continue, exit 0.
        let code = run_circuit_check(&cfg, cwd, "does-not-exist", 3, 1800, None);
        assert_eq!(code, 0);
    }

    #[test]
    fn stale_run_trips_on_stall() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path();
        let cfg = test_cfg(cwd);
        let now = state::now_secs();
        // last progress 2h ago, no failures → stall axis (idle_ttl 1800s) trips.
        let run = run_with("stale", vec![task("a", Status::Running, Some(now - 7200))]);
        run.save(&cfg, cwd).unwrap();
        let code = run_circuit_check(&cfg, cwd, "stale", 3, 1800, None);
        assert_eq!(code, 1);
        let recs =
            crate::gatelog::load_circuit_records(&state::project_state_dir(&cfg, cwd), "stale");
        assert_eq!(recs[0].reason.as_deref(), Some("stall"));
    }
}
