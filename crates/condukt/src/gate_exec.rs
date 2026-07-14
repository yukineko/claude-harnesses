//! REMOVE-GATE execute-vs-escalate: deterministic "may this gated task
//! auto-execute?" decision core.
//!
//! When condukt is about to execute a task that carries a gate, it must decide
//! whether the action is safe enough to run autonomously or whether it must
//! escalate to a human. This module is the deterministic core consulted before
//! executing a gated task: given the action's graded risk
//! ([`blastguard::classify::Risk`]), whether it is reversible, and whether the
//! prevailing run policy is `auto`, it returns a two-state verdict. The rule is
//! deliberately conservative — only the clearly-safe corner (Low risk AND
//! reversible AND policy auto) auto-executes; every irreversible or high-risk
//! gated action always escalates.
//!
//! Purity guarantee (mirrors [`crate::run_policy::decide_run_policy`] and
//! [`crate::circuit::decide_circuit`]): no filesystem, no `std::time`, no env,
//! no LLM. Same inputs always yield the same output, and it never panics.

use crate::config::Config;
use crate::state;
use blastguard::classify::{classify_change, Risk, RiskAssessment};
use blastguard::diffrisk::SensitiveConfig;
use std::path::Path;

/// The two-state verdict emitted by [`decide_gate_exec`]: run the gated task
/// autonomously, or escalate it to a human.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateExec {
    /// The action is clearly safe — auto-execute without human sign-off.
    AutoExec,
    /// The action is risky, irreversible, or policy-gated — escalate to a human.
    Escalate,
}

/// Decide whether a gated task may auto-execute or must escalate.
///
/// Pure and deterministic: no LLM, no network, no filesystem, no clock. Never
/// panics. Returns [`GateExec::AutoExec`] **if and only if** ALL THREE hold:
/// `risk` is [`Risk::Low`] AND `reversible == true` AND `policy_is_auto ==
/// true`. In every other case — Medium/High risk, not reversible, or a policy
/// that is not auto — it returns [`GateExec::Escalate`]. Conservative by design:
/// only the clearly-safe corner auto-execs.
pub fn decide_gate_exec(risk: Risk, reversible: bool, policy_is_auto: bool) -> GateExec {
    if matches!(risk, Risk::Low) && reversible && policy_is_auto {
        GateExec::AutoExec
    } else {
        GateExec::Escalate
    }
}

// ── signal gathering + the CLI handler (clock/FS/env live HERE, not the core) ─

/// A stable lowercase slug for a graded risk (mirrors blastguard's own
/// serde `rename_all = "lowercase"`), used for the JSON output + journal.
fn risk_slug(risk: Risk) -> &'static str {
    match risk {
        Risk::Low => "low",
        Risk::Medium => "medium",
        Risk::High => "high",
    }
}

/// Gather the `{risk, reversible}` signals for one gated task, FAIL-SOFT: an
/// unloadable run, a missing/corrupt decomposition, or a task id that isn't in
/// the decomposition yields `None` (which the caller degrades to Escalate).
/// Never panics. Classifies the SAME action text the schedule force-gate does
/// via [`crate::schedule::task_action_text`] — no duplicated join logic.
fn gather_assessment(
    cfg: &Config,
    cwd: &Path,
    run_id: &str,
    task_id: &str,
) -> Option<RiskAssessment> {
    let raw = state::load_decomposition(cfg, cwd, run_id).ok()?;
    let dec = serde_json::from_str::<crate::model::Decomposition>(&raw).ok()?;
    let task = dec.tasks.iter().find(|t| t.id == task_id)?;
    // BOUNDED SCOPE: at gate-check time there is no diff yet (the task hasn't
    // executed), so we only wire the sensitive-path glob signal (which needs
    // only `paths`) via `classify_change`. The public-symbol-diff signal
    // legitimately cannot fire here (empty diff_text) and simply won't —
    // matching classify_change's documented additive/backward-compatible
    // behavior. `touched_files` is condukt's own task field, so this is free.
    let sensitive = SensitiveConfig::default();
    Some(classify_change(
        &crate::schedule::task_action_text(task),
        &task.touched_files,
        "",
        &sensitive,
    ))
}

/// Handler for `condukt gate check --run RID --task TASKID`. Gathers the
/// `{risk, reversible}` signals FAIL-SOFT (any gathering error degrades to
/// Escalate and never panics), derives `policy_is_auto` from the SAME autonomy
/// predicate `state autonomy-check` uses, runs the pure [`decide_gate_exec`]
/// core, and prints the verdict + gathered signals as JSON on stdout.
///
/// On [`GateExec::AutoExec`] it FIRST writes a durable run checkpoint (so the
/// action is recoverable), THEN appends the decision to the fail-soft
/// append-only journal, THEN returns exit `0`. On [`GateExec::Escalate`] it
/// appends the decision (fail-soft) and returns exit `1`, preserving the human
/// stop so a caller can `if ! condukt gate check --run RID --task T; then
/// escalate; fi`.
pub fn run_gate_check(cfg: &Config, cwd: &Path, run_id: &str, task_id: &str) -> i32 {
    let assessment = gather_assessment(cfg, cwd, run_id, task_id);
    let policy_is_auto = crate::policy_is_autonomous(cfg);

    // Fail-soft: a missing signal (assessment None) degrades to Escalate.
    let verdict = match &assessment {
        Some(a) => decide_gate_exec(a.risk, a.reversible, policy_is_auto),
        None => GateExec::Escalate,
    };
    let verdict_str = match verdict {
        GateExec::AutoExec => "auto_exec",
        GateExec::Escalate => "escalate",
    };
    let risk = assessment.as_ref().map(|a| risk_slug(a.risk).to_string());
    let reversible = assessment.as_ref().map(|a| a.reversible);

    // Observable stdout JSON (verdict + every gathered signal + the task id).
    let out = serde_json::json!({
        "verdict": verdict_str,
        "risk": risk,
        "reversible": reversible,
        "policy_is_auto": policy_is_auto,
        "task": task_id,
    });
    println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());

    let dir = state::project_state_dir(cfg, cwd);

    // On AutoExec: checkpoint FIRST so the auto-executed action is recoverable,
    // reusing the exact function `state checkpoint` calls. Fail-soft: an
    // unloadable run or a checkpoint write error must not change the exit code.
    if matches!(verdict, GateExec::AutoExec) {
        if let Ok(rs) = state::RunState::load(cfg, cwd, run_id) {
            let shas = crate::capture_branch_shas(cwd, &rs);
            let _ = crate::checkpoint::write_checkpoint(&dir, run_id, &rs, "gate-exec", shas);
        }
    }

    // Journal the decision to the append-only JSONL trail in BOTH cases —
    // FAIL-SOFT (a journaling failure must never change the exit code).
    let record = crate::gatelog::GateExecRecord {
        verdict: verdict_str.to_string(),
        task: task_id.to_string(),
        risk: risk.clone(),
        reversible,
        policy_is_auto,
        recorded_at: state::now_secs(),
    };
    crate::gatelog::append_gate_exec(&dir, run_id, &record);

    // On Escalate ONLY: also record a durable overwatch review-finding so a
    // needs-human verdict reaches `overwatch review-queue`'s ai-finding stream
    // automatically (rather than only living in this run's local journal).
    // The finding-id is keyed on (run_id, task_id) so re-checking the SAME gate
    // under codegen flood collapses to one queue row (idempotent), not one row
    // per invocation. FAIL-SOFT: a recording failure must never change the
    // returned exit code, stdout, or the journal above — identical behavior
    // whether or not the finding write succeeds.
    if matches!(verdict, GateExec::Escalate) {
        let finding_id = format!("gate-exec:{run_id}:{task_id}");
        let severity = if risk.as_deref() == Some("high") {
            "high"
        } else {
            "medium"
        };
        let summary = format!(
            "gate-check escalated: task {task_id} risk={} reversible={} policy_is_auto={}",
            risk.as_deref().unwrap_or("unknown"),
            reversible
                .map(|b| b.to_string())
                .unwrap_or_else(|| "unknown".to_string()),
            policy_is_auto,
        );
        let _ = overwatch::store::record_finding(
            cwd,
            finding_id,
            "condukt-gate".to_string(),
            Some(severity.to_string()),
            summary,
            None,
            None,
        );
    }

    match verdict {
        GateExec::AutoExec => 0,
        GateExec::Escalate => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_exec_low_reversible_auto_autoexecs() {
        assert_eq!(
            decide_gate_exec(Risk::Low, true, true),
            GateExec::AutoExec,
            "the only safe corner (Low + reversible + auto) must auto-exec"
        );
    }

    #[test]
    fn gate_exec_low_reversible_not_auto_escalates() {
        assert_eq!(
            decide_gate_exec(Risk::Low, true, false),
            GateExec::Escalate,
            "policy not auto must escalate even when Low + reversible"
        );
    }

    #[test]
    fn gate_exec_low_not_reversible_escalates() {
        assert_eq!(
            decide_gate_exec(Risk::Low, false, true),
            GateExec::Escalate,
            "not reversible must escalate even when Low + auto"
        );
    }

    #[test]
    fn gate_exec_medium_reversible_auto_escalates() {
        assert_eq!(
            decide_gate_exec(Risk::Medium, true, true),
            GateExec::Escalate,
            "Medium risk must escalate"
        );
    }

    #[test]
    fn gate_exec_high_reversible_auto_escalates() {
        assert_eq!(
            decide_gate_exec(Risk::High, true, true),
            GateExec::Escalate,
            "High risk must escalate"
        );
    }

    #[test]
    fn gate_exec_high_not_reversible_auto_escalates() {
        assert_eq!(
            decide_gate_exec(Risk::High, false, true),
            GateExec::Escalate,
            "High + irreversible must escalate"
        );
    }

    #[test]
    fn gate_exec_is_deterministic() {
        let v1 = decide_gate_exec(Risk::Low, true, true);
        let v2 = decide_gate_exec(Risk::Low, true, true);
        assert_eq!(v1, v2, "same inputs must yield the same verdict");
    }
}
