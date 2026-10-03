//! `condukt gate reconcile-findings` — close condukt-gate review findings
//! (`gate-exec:<run_id>:<task_id>`, recorded by [`crate::gate_exec`] on an
//! escalation) by OBSERVING the run state, never by commit-message self-report
//! (backlog 89544915, rulings R1/R2/R5/R6).
//!
//! Per open finding:
//!
//! * run state loads, the task is present and its status is terminal
//!   (`done` | `verified` | `cancelled` | `discarded`) → closed with the
//!   non-human verdict `resolved`, evidence naming run, task and status;
//! * status `pending` / `running` / `failed` → still present (open, not an
//!   error);
//! * run state present but unreadable / undecodable, or the task id is not in
//!   a loaded run → UNDETERMINED (open, reason reported, exit 3). Corrupt state
//!   is never aged out;
//! * run state ABSENT → UNDETERMINED until the absence has itself been
//!   OBSERVED for [`ABSENCE_GRACE_SECS`]: the first reconcile that sees the
//!   absence records the instant in a per-project ledger, and only a later
//!   reconcile that still sees it absent ≥ 30 days after that stamp closes the
//!   finding (`absent since <epoch>`). The age is never guessed from the
//!   finding's own timestamp or a file mtime. A run state that reappears
//!   clears the stamp (the absence was not continuous).
//!
//! The clock is `CONDUKT_NOW_EPOCH` when set (test seam), else the real clock;
//! an unparseable `CONDUKT_NOW_EPOCH` makes every clock-dependent judgement
//! undetermined rather than falling back to some other instant.
use crate::config::Config;
use crate::state::{self, RunState, Status};
use harness_core::verdict::Determination;
use overwatch::observed_close::{self, Observation, ReconcileReport};
use overwatch::review_finding::ReviewFinding;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// How long an absent run state must have been OBSERVED absent before its
/// findings age out (ruling R6: N = 30 days).
pub const ABSENCE_GRACE_SECS: i64 = 30 * 86_400;

const PREFIX: &str = "gate-exec:";
const REVIEWER: &str = "condukt-reconcile";
const OBSERVED_SOURCE: &str = "condukt run-state";

/// The "now" this reconcile judges against.
fn now_epoch() -> Determination<i64> {
    match std::env::var("CONDUKT_NOW_EPOCH") {
        Err(std::env::VarError::NotPresent) => Determination::known(state::now_secs()),
        Err(e) => Determination::undetermined(format!("CONDUKT_NOW_EPOCH unreadable: {e}")),
        Ok(raw) => match raw.trim().parse::<i64>() {
            Ok(n) => Determination::known(n),
            Err(e) => Determination::undetermined(format!(
                "CONDUKT_NOW_EPOCH={raw:?} is not a unix-seconds integer: {e}"
            )),
        },
    }
}

/// Where the first-absence stamps live: a sub-directory of the per-project
/// state dir (a sub-directory, so `state::all_runs`'s top-level `*.json` scan
/// never mistakes it for a run).
fn absence_ledger_path(cfg: &Config, cwd: &Path) -> PathBuf {
    state::project_state_dir(cfg, cwd)
        .join("finding-reconcile")
        .join("absence.json")
}

/// `finding_id -> first observed-absent epoch`. Tri-state: absent file =
/// no stamps yet; unreadable / undecodable = undetermined (an absence age we
/// cannot read is not "zero days").
fn load_absence(path: &Path) -> Determination<BTreeMap<String, i64>> {
    match harness_core::boundary::read_to_string(path) {
        Determination::Known(None) => Determination::known(BTreeMap::new()),
        Determination::Known(Some(txt)) => match serde_json::from_str(&txt) {
            Ok(m) => Determination::known(m),
            Err(e) => Determination::undetermined(format!(
                "absence ledger {} is undecodable: {e}",
                path.display()
            )),
        },
        Determination::Undetermined(why) => Determination::Undetermined(why),
    }
}

fn save_absence(path: &Path, map: &BTreeMap<String, i64>) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!("json.tmp.{}", std::process::id()));
    std::fs::write(&tmp, serde_json::to_string_pretty(map)?)?;
    std::fs::rename(&tmp, path)
}

/// What the run-state file for `run_id` looks like right now.
enum RunObservation {
    Absent,
    Loaded(RunState),
}

fn observe_run(cfg: &Config, cwd: &Path, run_id: &str) -> Determination<RunObservation> {
    let path = state::run_state_path(cfg, cwd, run_id);
    match harness_core::boundary::read_to_string(&path) {
        Determination::Known(None) => Determination::known(RunObservation::Absent),
        Determination::Known(Some(txt)) => match serde_json::from_str::<RunState>(&txt) {
            Ok(rs) => Determination::known(RunObservation::Loaded(rs)),
            Err(e) => Determination::undetermined(format!(
                "run state {} is present but undecodable ({e}); corrupt state is never aged out",
                path.display()
            )),
        },
        Determination::Undetermined(why) => Determination::undetermined(format!(
            "run state {} could not be read: {}",
            path.display(),
            why.as_str()
        )),
    }
}

fn status_label(s: Status) -> &'static str {
    match s {
        Status::Pending => "pending",
        Status::Running => "running",
        Status::Done => "done",
        Status::Failed => "failed",
        Status::Verified => "verified",
        Status::Cancelled => "cancelled",
        Status::Discarded => "discarded",
    }
}

/// Terminal = the escalated task's question is settled. `Failed` is NOT: a
/// failed task is still waiting on someone.
fn is_terminal(s: Status) -> bool {
    match s {
        Status::Done | Status::Verified | Status::Cancelled | Status::Discarded => true,
        Status::Pending | Status::Running | Status::Failed => false,
    }
}

/// Split `gate-exec:<run_id>:<task_id>`.
fn parse_id(id: &str) -> Option<(&str, &str)> {
    let rest = id.strip_prefix(PREFIX)?;
    let (run, task) = rest.split_once(':')?;
    (!run.is_empty() && !task.is_empty()).then_some((run, task))
}

/// Run one reconcile pass for the project at `cwd`.
pub fn reconcile(cfg: &Config, cwd: &Path) -> ReconcileReport {
    let open = match observed_close::open_findings(cwd, |id| id.starts_with(PREFIX)) {
        Determination::Known(v) => v,
        Determination::Undetermined(why) => {
            return ReconcileReport::store_undetermined(why.as_str())
        }
    };
    if open.is_empty() {
        return ReconcileReport::default();
    }
    let now = now_epoch();
    let ledger_path = absence_ledger_path(cfg, cwd);
    let mut ledger = load_absence(&ledger_path);
    // Ids whose stamp this pass ADDED (first sighting) or CLEARED (state
    // reappeared). Both changes must be persisted for the judgement to stand.
    let mut stamped: Vec<String> = Vec::new();
    let mut cleared: Vec<String> = Vec::new();

    let judge = |f: &ReviewFinding| -> Determination<Observation> {
        let Some((run_id, task_id)) = parse_id(&f.finding_id) else {
            return Determination::undetermined(format!(
                "finding id {:?} is not `gate-exec:<run_id>:<task_id>`",
                f.finding_id
            ));
        };
        match observe_run(cfg, cwd, run_id) {
            Determination::Undetermined(why) => Determination::Undetermined(why),
            Determination::Known(RunObservation::Loaded(rs)) => {
                // Present again: an earlier absence was not continuous.
                if let Determination::Known(m) = &mut ledger {
                    if m.remove(&f.finding_id).is_some() {
                        cleared.push(f.finding_id.clone());
                    }
                }
                match rs.tasks.iter().find(|t| t.id == task_id) {
                    None => Determination::undetermined(format!(
                        "task {task_id} is not in run {run_id} (run state loaded, task absent)"
                    )),
                    Some(t) if is_terminal(t.status) => {
                        Determination::known(Observation::Resolved {
                            evidence: format!(
                                "run {run_id} task {task_id} status {}",
                                status_label(t.status)
                            ),
                        })
                    }
                    Some(_) => Determination::known(Observation::StillPresent),
                }
            }
            Determination::Known(RunObservation::Absent) => {
                let now = match &now {
                    Determination::Known(n) => *n,
                    Determination::Undetermined(why) => {
                        return Determination::undetermined(format!(
                            "run {run_id} state absent and the clock is undetermined: {}",
                            why.as_str()
                        ))
                    }
                };
                let m = match &mut ledger {
                    Determination::Known(m) => m,
                    Determination::Undetermined(why) => {
                        return Determination::undetermined(format!(
                            "run {run_id} state absent, but the first-absence ledger cannot be \
                             read, so the absence age is unknown: {}",
                            why.as_str()
                        ))
                    }
                };
                match m.get(&f.finding_id).copied() {
                    None => {
                        m.insert(f.finding_id.clone(), now);
                        stamped.push(f.finding_id.clone());
                        Determination::undetermined(format!(
                            "run {run_id} state absent; absence first observed now ({now}); \
                             closes only after it has been observed absent for 30 days"
                        ))
                    }
                    Some(since) if now.saturating_sub(since) >= ABSENCE_GRACE_SECS => {
                        Determination::known(Observation::Resolved {
                            evidence: format!(
                                "run {run_id} task {task_id}: run state absent since {since} \
                                 (first observed absent), still absent at {now}"
                            ),
                        })
                    }
                    Some(since) => Determination::undetermined(format!(
                        "run {run_id} state absent since {since}; {}s observed, 30 days ({}s) required",
                        now.saturating_sub(since),
                        ABSENCE_GRACE_SECS
                    )),
                }
            }
        }
    };
    let resolved_ts = match &now {
        Determination::Known(n) => *n,
        Determination::Undetermined(_) => state::now_secs(),
    };
    let mut report =
        observed_close::close_observed(cwd, open, judge, REVIEWER, OBSERVED_SOURCE, resolved_ts);

    if let Determination::Known(m) = &mut ledger {
        // Stamps of findings closed this pass are no longer needed.
        let mut pruned = false;
        for id in &report.resolved {
            pruned |= m.remove(id).is_some();
        }
        if !stamped.is_empty() || !cleared.is_empty() || pruned {
            if let Err(e) = save_absence(&ledger_path, m) {
                let note = format!(
                    "the first-absence ledger {} could NOT be written ({e})",
                    ledger_path.display()
                );
                // A first sighting that was not persisted was not recorded.
                for u in report.undetermined.iter_mut() {
                    if stamped.contains(&u.finding_id) {
                        u.why = format!("{}; {note}", u.why);
                    }
                }
                // A cleared stamp that was not persisted survives on disk and
                // would let a LATER absence age out from the old instant: the
                // still-present judgement cannot be left looking clean.
                for id in cleared {
                    report.undetermined.push(observed_close::NotClosed {
                        finding_id: id,
                        why: format!(
                            "run state reappeared, but its stale absence stamp could not be \
                             cleared: {note}"
                        ),
                    });
                }
            }
        }
    }
    report
}

/// CLI entry: run, emit, return the exit code (0 / 3).
pub fn run_cli(cfg: &Config, cwd: &Path, json: bool) -> i32 {
    reconcile(cfg, cwd).emit("condukt gate reconcile-findings", json)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_id_splits_run_and_task() {
        assert_eq!(parse_id("gate-exec:run-1:t2"), Some(("run-1", "t2")));
        assert_eq!(parse_id("gate-exec:run-1"), None);
        assert_eq!(parse_id("gate-exec::t"), None);
        assert_eq!(parse_id("record-audit:x:1"), None);
    }

    #[test]
    fn failed_is_not_terminal() {
        assert!(!is_terminal(Status::Failed));
        assert!(is_terminal(Status::Discarded));
    }
}
