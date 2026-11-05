//! Outcome recording (fugu-router learning signal): the pure join of a run's
//! task states with its decomposition into the episodes `record-run` emits.
//! Split out of `state.rs` (file-growth cap); re-exported from there.

use super::{RunState, Status};

/// One outcome to record to fugu-router, derived by joining a run's task states
/// with its decomposition. Pure data so the join logic is unit-testable without
/// spawning the fugu-router binary.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordSpec {
    /// The run task id — the stable half of the `recorded_episodes` key.
    pub task_id: String,
    pub title: String,
    pub files: Vec<String>,
    pub class: String,
    /// Model the task ran on: the task state's recorded model if present, else
    /// the decomposition's `suggested_model`, else a conservative default.
    pub model: String,
    /// "verified" or "failed" — the fugu-router status vocabulary.
    pub status: String,
    /// The manually-set cost (`state set --cost`), if any. `None` = never set,
    /// which the caller must treat as UNMEASURED (omit `fugu-router record
    /// --cost`), never as a fabricated `0.0` (backlog 55fb3861). An explicit
    /// `--cost 0` is `Some(0.0)` and is forwarded as given.
    pub cost_usd: Option<f64>,
    pub done_criteria: Option<String>,
    /// Wall-clock seconds between `started_at` (last `running` transition) and
    /// `updated_at` (the settling transition), when both are known. `None`
    /// when either timestamp is missing (legacy task-state, or a task that
    /// somehow settled without ever recording a `running` transition) — never
    /// fabricated as 0.0, so the caller can omit `--duration` outright.
    pub duration_secs: Option<f64>,
    /// The Task-tool agentId supplied via `state set --agent-id`, if any. When
    /// present, the caller (record_runs) attempts to resolve the real cost from
    /// `gauge subagents` by exact ID match, overriding `cost_usd` on success.
    pub agent_id: Option<String>,
    /// Routing-decision provenance (`basis`/`confidence`/`rationale` from
    /// `route.json`) and measured lines-changed, supplied via `state set
    /// --route-basis/--route-confidence/--route-rationale/--lines-added/
    /// --lines-removed`. Measurement only — passed through to the fugu-router
    /// episode unchanged; never consulted by condukt itself.
    pub route_basis: Option<String>,
    /// The decomposition task's `suggested_model` (independent of `model`,
    /// which is what actually ran). `None` = no suggestion / empty string:
    /// the caller omits `--suggested-model` entirely, never sends `""`.
    pub suggested_model: Option<String>,
    pub route_confidence: Option<String>,
    pub route_rationale: Option<String>,
    pub lines_added: Option<u64>,
    pub lines_removed: Option<u64>,
    /// Verifier-side model/cost/agent-id, supplied via `state set
    /// --verifier-model/--verifier-cost/--verifier-agent-id`. Same
    /// provenance/measurement-only caveats as the worker-side `model`/
    /// `cost_usd`/`agent_id` fields above — passed through to the fugu-router
    /// episode unchanged; never consulted by condukt itself. `verifier_model`
    /// being `Some` is also the signal `record_runs` uses to emit a SECOND
    /// episode with `role=verifier`, reusing this spec's title/files/class/
    /// done_criteria/status.
    pub verifier_model: Option<String>,
    pub verifier_cost_usd: Option<f64>,
    pub verifier_agent_id: Option<String>,
}

/// `record` argv for the episode's role: `--role <role>`, plus `--suggested-model
/// <m>` only for the worker episode and only when known (otherwise the flag is
/// omitted, never sent as `""`). The verifier episode never carries it: the
/// suggestion is for the worker's model, so tagging the verifier's episode with
/// it would misattribute.
pub fn role_args<'a>(s: &'a RecordSpec, role: &'a str) -> Vec<&'a str> {
    match (role, s.suggested_model.as_deref()) {
        ("worker", Some(m)) => vec!["--role", role, "--suggested-model", m],
        _ => vec!["--role", role],
    }
}

/// Build the outcomes to record for a run, or `None` when the run is not yet
/// recordable. A run is recordable when:
/// - it has not already been recorded (`recorded_at` is `None`), and
/// - every task has reached a settled state (verified/failed/cancelled) — i.e.
///   nothing is still pending/running and could yet change.
///
/// Only `verified` and `failed` tasks produce a record (a `cancelled` task was
/// abandoned by the user and carries no learning signal). The returned vec may
/// be empty (e.g. an all-cancelled run); the caller still marks it recorded so
/// repeated hook firings converge.
pub fn records_for_run(
    run: &RunState,
    dec: &crate::model::Decomposition,
) -> Option<Vec<RecordSpec>> {
    if run.recorded_at.is_some() || run.tasks.is_empty() {
        return None;
    }
    let settled = run.tasks.iter().all(|t| {
        matches!(
            t.status,
            Status::Verified | Status::Failed | Status::Cancelled | Status::Discarded
        )
    });
    if !settled {
        return None;
    }

    let by_id: std::collections::HashMap<&str, &crate::model::Task> =
        dec.tasks.iter().map(|t| (t.id.as_str(), t)).collect();

    let specs = run
        .tasks
        .iter()
        .filter_map(|ts| {
            let status = match ts.status {
                Status::Verified => "verified",
                Status::Failed => "failed",
                _ => return None, // cancelled / non-terminal: no learning signal
            };
            let task = by_id.get(ts.id.as_str());
            let title = task
                .map(|t| t.title.clone())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| ts.id.clone());
            let files = task.map(|t| t.touched_files.clone()).unwrap_or_default();
            let class = task
                .map(|t| format!("{:?}", t.class).to_lowercase())
                .unwrap_or_else(|| "parallel".to_string());
            let model = ts
                .model
                .clone()
                .or_else(|| task.and_then(|t| t.suggested_model.clone()))
                .unwrap_or_else(|| "sonnet".to_string());
            let done_criteria = task.and_then(|t| t.done_criteria.clone());
            let duration_secs = match (ts.started_at, ts.updated_at) {
                (Some(started), Some(settled)) if settled >= started => {
                    Some((settled - started) as f64)
                }
                _ => None,
            };
            Some(RecordSpec {
                task_id: ts.id.clone(),
                title,
                files,
                class,
                model,
                status: status.to_string(),
                cost_usd: ts.cost_usd,
                done_criteria,
                duration_secs,
                agent_id: ts.agent_id.clone(),
                route_basis: ts.route_basis.clone(),
                suggested_model: task
                    .and_then(|t| t.suggested_model.clone())
                    .filter(|m| !m.trim().is_empty()),
                route_confidence: ts.route_confidence.clone(),
                route_rationale: ts.route_rationale.clone(),
                lines_added: ts.lines_added,
                lines_removed: ts.lines_removed,
                verifier_model: ts.verifier_model.clone(),
                verifier_cost_usd: ts.verifier_cost_usd,
                verifier_agent_id: ts.verifier_agent_id.clone(),
            })
        })
        .collect();
    Some(specs)
}
