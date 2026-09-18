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
//! The idle signal is a [`Determination<i64>`], not an `i64`: "this run last
//! progressed 30s ago" and "how idle this run is cannot be measured at all" are
//! different answers, and they are kept apart all the way into the emitted
//! JSON, where the second one is `null`. Collapsing the second into a confident
//! `0` is what this module used to do, and a `0` reads as "it progressed this
//! very second" — the non-trip side. See [`CircuitReason::IdleUnmeasured`].
//!
//! Purity guarantee (mirrors [`crate::run_policy::decide_run_policy`] and
//! [`crate::policy::decide`]): no filesystem, no `std::time`, no env, no LLM.
//! The caller gathers the signals; this function is a total, deterministic
//! function of its arguments and never panics. The caps are opt-out: a
//! `streak_cap` of 0 disables the streak condition, and an `idle_ttl_secs` of 0
//! disables the stall condition.

use crate::config::Config;
use crate::state::{self, RunState, Status};
use harness_core::verdict::{Determination, Required};
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
    /// How idle the run is could **not be measured** (no task carries an
    /// `updated_at`, or the run state could not be loaded) while the idle axis
    /// was switched on.
    ///
    /// # Why this trips rather than continuing
    ///
    /// This is the fail-open named in backlog 53c825f6. "Cannot determine how
    /// idle this run is" is not "this run is not idle" — but the old
    /// `.unwrap_or(0)` mapped it onto the very same `0` that a run which
    /// progressed this second produces, i.e. onto the most permissive value the
    /// axis has. Per CLAUDE.md #3 an undetermined answer resolves to the
    /// RESTRICTIVE side, so it trips.
    ///
    /// # Why it is its own reason and not `Stall`
    ///
    /// Reusing `Stall` would make "observed: this run has not progressed for
    /// 1800s" and "could not observe whether this run progressed at all"
    /// indistinguishable in the journal — the same collapse one level down.
    /// The restrictive VERDICT and the honest LABEL are both required; this
    /// variant is the deliberate third outcome.
    IdleUnmeasured,
}

impl CircuitReason {
    /// A stable lowercase slug for journaling (`"failure_streak"` /
    /// `"budget_over_cap"` / `"stall"` / `"idle_unmeasured"`).
    pub fn as_str(&self) -> &'static str {
        match self {
            CircuitReason::FailureStreak => "failure_streak",
            CircuitReason::BudgetOverCap => "budget_over_cap",
            CircuitReason::Stall => "stall",
            CircuitReason::IdleUnmeasured => "idle_unmeasured",
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
/// 3. else `idle_ttl_secs <= 0` → `Continue` (the idle axis is opted out).
/// 4. else `idle` is `Known(secs)` with `secs >= idle_ttl_secs` → `Trip(Stall)`.
/// 5. else `idle` is `Undetermined` → `Trip(IdleUnmeasured)`.
/// 6. otherwise → `Continue`.
///
/// A `streak_cap` of 0 disables the streak condition; an `idle_ttl_secs` of 0
/// disables the idle axis. These opt-outs keep the function total.
///
/// # The idle axis is TRI-STATE on purpose
///
/// `idle` is a [`Determination<i64>`] so that "could not measure" arrives as
/// its own input rather than as a `0`. Step 5 is the fail-closed resolution
/// CLAUDE.md #3 requires: an unanswered "is this run idle?" must not be counted
/// as a non-trip. It is deliberately labelled [`CircuitReason::IdleUnmeasured`]
/// rather than `Stall`, so the journal never claims an observation that was
/// never made.
///
/// Step 3 is checked BEFORE the determination is read, and that ordering is the
/// one place an undetermined idle does not trip: `idle_ttl_secs == 0` means the
/// operator switched the idle axis off, so there is no unanswered question to
/// fail closed on — exactly as a `Known(i64::MAX)` does not trip with the axis
/// off either. That arm is an explicit opt-out, not a permissive default
/// standing in for an unknown.
pub fn decide_circuit(
    streak: u32,
    streak_cap: u32,
    budget_over_cap: bool,
    idle: Determination<i64>,
    idle_ttl_secs: i64,
) -> CircuitVerdict {
    if budget_over_cap {
        CircuitVerdict::Trip(CircuitReason::BudgetOverCap)
    } else if streak_cap > 0 && streak >= streak_cap {
        CircuitVerdict::Trip(CircuitReason::FailureStreak)
    } else if idle_ttl_secs <= 0 {
        // The idle axis is switched off entirely (see the docstring): neither a
        // measured nor an unmeasurable idle is consulted.
        CircuitVerdict::Continue
    } else {
        // `require()` is `Determination`'s only extractor and offers no
        // `unwrap_or`, so the undetermined arm has to be written out — which
        // is the point: it is stated here, in the diff, not defaulted away.
        match idle.require() {
            Required::Determined(secs) if secs >= idle_ttl_secs => {
                CircuitVerdict::Trip(CircuitReason::Stall)
            }
            Required::Determined(_) => CircuitVerdict::Continue,
            Required::Blocked(_) => CircuitVerdict::Trip(CircuitReason::IdleUnmeasured),
        }
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

/// The idle signal as a TRI-STATE: how long the run has gone without progress,
/// or *why that could not be measured*. `now` is injected so this is pure and
/// deterministic (the clock is read by the caller, at the CLI layer).
///
/// * `run` is `None` (the run state could not be loaded) → `Undetermined`.
/// * the run carries no `updated_at` at all (no tasks, or legacy run-state
///   written before the field existed) → `Undetermined`.
/// * otherwise → `Known(now - latest_progress)`, clamped at 0.
///
/// The clamp is deliberately INSIDE the `Known` arm: a timestamp in the future
/// is clock skew over a real, observed progress transition, so it stays a
/// measurement (a negative idle would only ever mean "even fresher than now").
/// That is the one case where a `0` here is honest. The two `Undetermined`
/// arms are the cases the old `.unwrap_or(0)` used to render as that same `0`.
pub(crate) fn gather_idle_secs(run: Option<&RunState>, now: i64) -> Determination<i64> {
    let Some(run) = run else {
        return Determination::undetermined(
            "run state could not be loaded — time since last progress is unmeasurable",
        );
    };
    match max_updated_at(run) {
        Some(ts) => Determination::known((now - ts).max(0)),
        None => Determination::undetermined(
            "no task carries an updated_at — time since last progress is unmeasurable",
        ),
    }
}

/// The `(verdict, reason slug)` pair shared by the stdout JSON and the journal
/// record, so the two can never disagree about what was decided.
fn verdict_fields(verdict: &CircuitVerdict) -> (&'static str, Option<String>) {
    match verdict {
        CircuitVerdict::Continue => ("continue", None),
        CircuitVerdict::Trip(r) => ("trip", Some(r.as_str().to_string())),
    }
}

/// The observable stdout JSON of `condukt circuit check`: the verdict, its
/// reason slug, and every gathered signal.
///
/// `idle_secs` is `null` — never `0` — when idleness could not be measured, and
/// `idle_unknown_reason` then carries why. A consumer can therefore tell "this
/// run progressed this very second" (`idle_secs: 0`) from "nobody could tell
/// whether this run progressed" (`idle_secs: null`), which is the distinction
/// backlog 53c825f6 is about. The key name `idle_secs` is unchanged: nothing
/// outside this crate parses this object (the loops consult the exit code), so
/// widening the value to `null` is the whole change.
pub(crate) fn circuit_report(
    verdict: &CircuitVerdict,
    streak: u32,
    streak_cap: u32,
    budget_over_cap: bool,
    idle: &Determination<i64>,
    idle_ttl_secs: i64,
) -> serde_json::Value {
    let (verdict_str, reason) = verdict_fields(verdict);
    let (idle_secs, idle_unknown_reason) = match idle {
        Determination::Known(secs) => (serde_json::json!(secs), serde_json::Value::Null),
        Determination::Undetermined(why) => {
            (serde_json::Value::Null, serde_json::json!(why.as_str()))
        }
    };
    serde_json::json!({
        "verdict": verdict_str,
        "reason": reason,
        "streak": streak,
        "streak_cap": streak_cap,
        "budget_over_cap": budget_over_cap,
        "idle_secs": idle_secs,
        "idle_unknown_reason": idle_unknown_reason,
        "idle_ttl_secs": idle_ttl_secs,
    })
}

/// Handler for `condukt circuit check --run RID`. Gathers the three signals,
/// runs the pure [`decide_circuit`] core, prints the verdict + signals as JSON
/// on stdout, journals the same record fail-soft, and returns the process exit
/// code (`0` = continue, `1` = trip) so the loops can do
/// `if ! condukt circuit check --run RID; then stop; fi`. Never panics.
///
/// # Gathering is fail-soft on two axes and FAIL-CLOSED on the third
///
/// The streak and budget axes still degrade to their non-tripping value when
/// they cannot be read (an unloadable run has no countable failures; an absent
/// budget ledger or an unsupplied cap is not an over-cap observation).
///
/// The idle axis does NOT. It is gathered as a [`Determination<i64>`] by
/// [`gather_idle_secs`], and an idleness that could not be measured trips the
/// breaker as [`CircuitReason::IdleUnmeasured`] instead of entering the core as
/// a confident `0` (backlog 53c825f6). The visible consequences: a run id that
/// names no loadable run, and a run whose tasks carry no `updated_at` at all,
/// now exit `1` rather than `0`. Both mean "this gate cannot see whether the
/// loop is making progress", and a loop that keeps going on that answer is the
/// runaway the breaker exists to stop.
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

    // 3. idle — now minus the run's most-recent progress, as a TRI-STATE, so
    //    "could not measure" is NOT the same value as "progressed just now".
    //    Clock use is allowed HERE (CLI layer); only the pure core is
    //    clock-free, which is why `now` is injected into the gatherer.
    let idle = gather_idle_secs(run.as_ref(), state::now_secs());

    let verdict = decide_circuit(
        streak,
        streak_cap,
        budget_over_cap,
        idle.clone(),
        idle_ttl_secs,
    );
    let (verdict_str, reason) = verdict_fields(&verdict);

    // Observable stdout JSON (verdict + reason slug + every gathered signal).
    let out = circuit_report(
        &verdict,
        streak,
        streak_cap,
        budget_over_cap,
        &idle,
        idle_ttl_secs,
    );
    println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());

    // Journal the same record to the append-only JSONL trail — FAIL-SOFT: a
    // journaling failure must never change the exit code (mirrors gatelog).
    //
    // KNOWN RESIDUAL, stated rather than hidden: `CircuitRecord::idle_secs` is
    // an `i64` (it lives in `gatelog.rs`, outside this task's file scope), so
    // the unmeasurable case still has to journal SOME number and journals `0`.
    // That `0` is never unlabelled, and the label is what keeps it from being
    // read as a measurement: when the idle axis is on (`idle_ttl_secs > 0`) an
    // unmeasurable idle always trips, so the record reads
    // `verdict="trip", reason="idle_unmeasured"`; when the axis is off
    // (`idle_ttl_secs == 0`) the record says so in its own `idle_ttl_secs`
    // field and the idle signal was not consulted at all. A `continue` record
    // with `idle_ttl_secs > 0` therefore always carries a real measurement.
    // Pinned by `run_without_any_timestamp_trips_as_unmeasured_not_silent_continue`.
    // Widening the journal field to `Option<i64>` is the honest fix and is
    // reported as follow-up work.
    let journaled_idle_secs = match &idle {
        Determination::Known(secs) => *secs,
        Determination::Undetermined(_) => 0,
    };
    let record = crate::gatelog::CircuitRecord {
        verdict: verdict_str.to_string(),
        reason,
        streak,
        streak_cap,
        budget_over_cap,
        idle_secs: journaled_idle_secs,
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
        let v = decide_circuit(0, 5, true, Determination::known(0), 60);
        assert_eq!(v, CircuitVerdict::Trip(CircuitReason::BudgetOverCap));
    }

    #[test]
    fn failure_streak_trips() {
        let v = decide_circuit(5, 5, false, Determination::known(0), 60);
        assert_eq!(v, CircuitVerdict::Trip(CircuitReason::FailureStreak));
    }

    #[test]
    fn stall_trips() {
        let v = decide_circuit(0, 5, false, Determination::known(60), 60);
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
        let v = decide_circuit(2, 5, false, Determination::known(30), 60);
        assert_eq!(v, CircuitVerdict::Continue);
    }

    // ── boundary equality ──────────────────────────────────────────────────

    #[test]
    fn streak_equal_to_cap_trips() {
        assert_eq!(
            decide_circuit(5, 5, false, Determination::known(0), 0),
            CircuitVerdict::Trip(CircuitReason::FailureStreak)
        );
    }

    #[test]
    fn streak_one_below_cap_does_not_trip() {
        assert_eq!(
            decide_circuit(4, 5, false, Determination::known(0), 0),
            CircuitVerdict::Continue
        );
    }

    #[test]
    fn idle_equal_to_ttl_trips() {
        assert_eq!(
            decide_circuit(0, 0, false, Determination::known(60), 60),
            CircuitVerdict::Trip(CircuitReason::Stall)
        );
    }

    #[test]
    fn idle_one_below_ttl_does_not_trip() {
        assert_eq!(
            decide_circuit(0, 0, false, Determination::known(59), 60),
            CircuitVerdict::Continue
        );
    }

    // ── disabling semantics (cap of 0 opts the axis out) ───────────────────

    #[test]
    fn streak_cap_zero_never_trips_on_streak() {
        // A huge streak but streak_cap == 0 disables the condition.
        assert_eq!(
            decide_circuit(u32::MAX, 0, false, Determination::known(0), 0),
            CircuitVerdict::Continue
        );
    }

    #[test]
    fn idle_ttl_zero_never_trips_on_stall() {
        // A huge idle but idle_ttl_secs == 0 disables the condition.
        assert_eq!(
            decide_circuit(0, 0, false, Determination::known(i64::MAX), 0),
            CircuitVerdict::Continue
        );
    }

    // ── precedence: budget beats streak beats stall ────────────────────────

    #[test]
    fn budget_beats_streak_and_stall() {
        // All three conditions hold; budget wins.
        let v = decide_circuit(10, 5, true, Determination::known(100), 60);
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
        let v = decide_circuit(10, 5, false, Determination::known(100), 60);
        assert_eq!(v, CircuitVerdict::Trip(CircuitReason::FailureStreak));
    }

    // ── determinism ────────────────────────────────────────────────────────

    #[test]
    fn decide_circuit_is_deterministic() {
        let v1 = decide_circuit(10, 5, false, Determination::known(100), 60);
        let v2 = decide_circuit(10, 5, false, Determination::known(100), 60);
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
            autonomy_source: harness_core::autonomy::Source::BuiltinDefault,
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
            ..Default::default()
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
    fn missing_run_trips_as_unmeasured_rather_than_continuing() {
        // WAS missing_run_fails_soft_to_continue_not_panic, which asserted
        // code == 0 with the comment "no run saved: streak 0, idle 0, budget
        // off → Continue, exit 0". That "idle 0" is the fabricated value
        // backlog 53c825f6 names: an unloadable run cannot be observed to have
        // progressed, and the old test pinned reading that as "it progressed
        // just now". Same input, opposite expectation - and still no panic.
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path();
        let cfg = test_cfg(cwd);
        let code = run_circuit_check(&cfg, cwd, "does-not-exist", 3, 1800, None);
        assert_eq!(
            code, 1,
            "an unloadable run's idleness is unmeasurable, which must not exit 0"
        );
        let recs = crate::gatelog::load_circuit_records(
            &state::project_state_dir(&cfg, cwd),
            "does-not-exist",
        );
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].reason.as_deref(), Some("idle_unmeasured"));
    }

    #[test]
    fn missing_run_with_the_idle_axis_opted_out_still_continues() {
        // The opt-out survives the fix: with idle_ttl_secs == 0 the idle axis
        // is off, so even an unloadable run continues (and still never panics).
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path();
        let cfg = test_cfg(cwd);
        let code = run_circuit_check(&cfg, cwd, "does-not-exist-either", 3, 0, None);
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

    // ── the idle signal is TRI-STATE: "could not measure" ≠ "just progressed" ─
    //
    // Regression pins for the fail-open named in backlog 53c825f6: the idle
    // signal used to be an `i64` gathered with `.unwrap_or(0)`, so a run whose
    // idleness could not be measured at all (no task carries an `updated_at`,
    // or the run state could not be loaded) entered the decision as a confident
    // `0` — "it progressed this very second" — and fell to the NON-TRIP side.

    #[test]
    fn unmeasurable_idle_trips_instead_of_continuing() {
        let v = decide_circuit(
            0,
            5,
            false,
            Determination::undetermined("no task carries an updated_at"),
            1800,
        );
        assert_eq!(v, CircuitVerdict::Trip(CircuitReason::IdleUnmeasured));
    }

    #[test]
    fn unmeasurable_idle_is_a_different_answer_from_a_measured_zero() {
        // A MEASURED 0 means the run progressed this very second → Continue.
        assert_eq!(
            decide_circuit(0, 5, false, Determination::known(0), 1800),
            CircuitVerdict::Continue
        );
        // "Could not measure" must NOT reach that same answer. This single
        // assertion is the ticket: before the fix both inputs were the i64 `0`
        // and were therefore indistinguishable to this function.
        assert_ne!(
            decide_circuit(
                0,
                5,
                false,
                Determination::undetermined("run state could not be loaded"),
                1800
            ),
            CircuitVerdict::Continue
        );
    }

    #[test]
    fn unmeasurable_idle_does_not_trip_when_the_idle_axis_is_opted_out() {
        // `idle_ttl_secs == 0` disables the idle AXIS — the operator declared
        // the question irrelevant, so there is no unanswered question to fail
        // closed on. Symmetric with `idle_ttl_zero_never_trips_on_stall`: a
        // measured `i64::MAX` does not trip either.
        assert_eq!(
            decide_circuit(0, 5, false, Determination::undetermined("no timestamp"), 0),
            CircuitVerdict::Continue
        );
    }

    #[test]
    fn budget_and_streak_still_outrank_an_unmeasurable_idle() {
        assert_eq!(
            decide_circuit(
                0,
                5,
                true,
                Determination::undetermined("no timestamp"),
                1800
            ),
            CircuitVerdict::Trip(CircuitReason::BudgetOverCap)
        );
        assert_eq!(
            decide_circuit(
                5,
                5,
                false,
                Determination::undetermined("no timestamp"),
                1800
            ),
            CircuitVerdict::Trip(CircuitReason::FailureStreak)
        );
    }

    #[test]
    fn idle_unmeasured_slug_is_stable_and_distinct_from_stall() {
        assert_eq!(CircuitReason::IdleUnmeasured.as_str(), "idle_unmeasured");
        assert_ne!(
            CircuitReason::IdleUnmeasured.as_str(),
            CircuitReason::Stall.as_str()
        );
    }

    // ── where the tri-state comes from (gathering, clock injected) ──────────

    #[test]
    fn gather_idle_is_known_from_the_latest_progress_timestamp() {
        let run = run_with("r", vec![task("a", Status::Running, Some(100))]);
        assert_eq!(gather_idle_secs(Some(&run), 160), Determination::known(60));
    }

    #[test]
    fn gather_idle_clamps_clock_skew_to_zero_but_stays_known() {
        // A timestamp in the future is still a real observation of progress.
        let run = run_with("r", vec![task("a", Status::Running, Some(200))]);
        assert_eq!(gather_idle_secs(Some(&run), 100), Determination::known(0));
    }

    #[test]
    fn gather_idle_is_undetermined_when_no_task_carries_a_timestamp() {
        let run = run_with("r", vec![task("a", Status::Running, None)]);
        match gather_idle_secs(Some(&run), 100) {
            Determination::Undetermined(why) => {
                assert!(
                    !why.as_str().is_empty(),
                    "an undetermined idle must carry why"
                );
            }
            Determination::Known(secs) => {
                panic!("a run with no updated_at must not yield a measurement ({secs})")
            }
        }
    }

    #[test]
    fn gather_idle_is_undetermined_when_the_run_could_not_be_loaded() {
        match gather_idle_secs(None, 100) {
            Determination::Undetermined(why) => {
                assert!(!why.as_str().is_empty());
            }
            Determination::Known(secs) => {
                panic!("an unloadable run must not yield a measurement ({secs})")
            }
        }
    }

    #[test]
    fn gather_idle_is_undetermined_for_a_run_with_no_tasks_at_all() {
        let run = run_with("r", vec![]);
        assert!(matches!(
            gather_idle_secs(Some(&run), 100),
            Determination::Undetermined(_)
        ));
    }

    // ── the emitted JSON: null, never a fabricated 0 ───────────────────────

    #[test]
    fn report_emits_null_idle_secs_when_idleness_was_not_measured() {
        let idle = Determination::undetermined("no task carries an updated_at");
        let v = decide_circuit(0, 5, false, idle.clone(), 1800);
        let out = circuit_report(&v, 0, 5, false, &idle, 1800);
        assert!(
            out["idle_secs"].is_null(),
            "unmeasured idleness must serialize as null, got {out}"
        );
        assert_eq!(out["verdict"], "trip");
        assert_eq!(out["reason"], "idle_unmeasured");
        assert_eq!(out["idle_unknown_reason"], "no task carries an updated_at");
    }

    #[test]
    fn report_emits_a_real_zero_as_zero_not_null() {
        // The other half of "distinguishable from a real 0".
        let idle = Determination::known(0);
        let v = decide_circuit(0, 5, false, idle.clone(), 1800);
        let out = circuit_report(&v, 0, 5, false, &idle, 1800);
        assert_eq!(out["idle_secs"], serde_json::json!(0));
        assert!(!out["idle_secs"].is_null());
        assert_eq!(out["verdict"], "continue");
        assert!(out["idle_unknown_reason"].is_null());
    }

    #[test]
    fn report_emits_a_measured_idle_unchanged() {
        let idle = Determination::known(42);
        let v = decide_circuit(0, 5, false, idle.clone(), 1800);
        let out = circuit_report(&v, 0, 5, false, &idle, 1800);
        assert_eq!(out["idle_secs"], serde_json::json!(42));
    }

    // ── wired end-to-end through the CLI handler ───────────────────────────

    #[test]
    fn run_without_any_timestamp_trips_as_unmeasured_not_silent_continue() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path();
        let cfg = test_cfg(cwd);
        // Legacy run-state: tasks exist, none carries `updated_at`.
        let run = run_with("no-ts", vec![task("a", Status::Running, None)]);
        run.save(&cfg, cwd).unwrap();
        let code = run_circuit_check(&cfg, cwd, "no-ts", 3, 1800, None);
        assert_eq!(
            code, 1,
            "unmeasurable idleness must not exit 0 (that is the fail-open)"
        );
        let recs =
            crate::gatelog::load_circuit_records(&state::project_state_dir(&cfg, cwd), "no-ts");
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].verdict, "trip");
        // The journaled record LABELS its unmeasured idle: `idle_secs` is an
        // `i64` in `gatelog::CircuitRecord` (outside this task's scope), so the
        // unknown case still writes a `0` there — but never an unlabelled one.
        // This pins the label that keeps that 0 from reading as a measurement.
        assert_eq!(recs[0].reason.as_deref(), Some("idle_unmeasured"));
    }
}
