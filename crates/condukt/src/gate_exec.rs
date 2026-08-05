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
use harness_core::verdict::{Determination, Required};
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

/// The three answers [`gather_assessment`] can give, kept distinct rather than
/// flattened to `Option<RiskAssessment>`: `Missing` (no assessment was
/// produced) and `Undetermined` ("the classification was attempted and could
/// not be measured", carrying why) used to erase to the same `None`. Both
/// still degrade to [`GateExec::Escalate`] in [`run_gate_check`] — the
/// restricted side, unchanged — but only `Undetermined` carries a reason,
/// which `run_gate_check` now surfaces instead of dropping.
///
/// KNOWN GAP — the split is complete for *classification*, not for *loading*.
/// `Missing` still merges two different answers: "there is genuinely nothing
/// to assess" (no such run/task) and "there is something, but it could not be
/// read" (an IO error, or a decomposition that does not parse). The merge is
/// upstream of this type: [`state::load_decomposition`] returns a plain
/// `Result` over `std::fs::read_to_string`, so ENOENT and a read failure are
/// indistinguishable by the time they arrive here. Not a fail-open — every
/// one of those paths escalates — but a load failure is reported with a `null`
/// `undetermined_reason` and `risk=unknown reversible=unknown`, i.e. as if
/// nothing needed assessing. Tracked as backlog `6d451627`; fixing it means
/// making the *loader* tri-state, not adding a fourth variant here.
enum GatherOutcome {
    /// A risk was measured.
    Assessed(RiskAssessment),
    /// No assessment was produced: no such run/decomposition/task, or a
    /// decomposition that could not be read or parsed. See the KNOWN GAP on
    /// [`GatherOutcome`] — these two are not yet distinguishable.
    Missing,
    /// The classification could not be determined; carries why.
    Undetermined(String),
}

/// Gather the `{risk, reversible}` signals for one gated task, FAIL-CLOSED: an
/// unloadable run, a missing/corrupt decomposition, or a task id that isn't in
/// the decomposition yields [`GatherOutcome::Missing`] (see the KNOWN GAP on
/// [`GatherOutcome`]: those are not distinguished from each other); **a risk
/// classification that could not be determined** yields
/// [`GatherOutcome::Undetermined`] with the reason. Both degrade to
/// [`GateExec::Escalate`] in the caller — the restricted side. Never panics.
/// Classifies the SAME action text the schedule force-gate does via
/// [`crate::schedule::task_action_text`] — no duplicated join logic.
///
/// The `Undetermined` arm matters: [`classify_change`] returns a
/// [`Determination`] because its sensitive-path signal can fail to compile
/// (a bad configured glob). Reading that as a permissive Low/reversible
/// assessment would auto-exec a task whose risk was never actually measured, so
/// the undetermined arm joins the existing missing-signal path to Escalate
/// rather than producing an assessment.
fn gather_assessment(cfg: &Config, cwd: &Path, run_id: &str, task_id: &str) -> GatherOutcome {
    let Ok(raw) = state::load_decomposition(cfg, cwd, run_id) else {
        return GatherOutcome::Missing;
    };
    let Ok(dec) = serde_json::from_str::<crate::model::Decomposition>(&raw) else {
        return GatherOutcome::Missing;
    };
    let Some(task) = dec.tasks.iter().find(|t| t.id == task_id) else {
        return GatherOutcome::Missing;
    };
    // BOUNDED SCOPE: at gate-check time there is no diff yet (the task hasn't
    // executed), so we only wire the sensitive-path glob signal (which needs
    // only `paths`) via `classify_change`. The public-symbol-diff signal
    // legitimately cannot fire here (empty diff_text) and simply won't —
    // matching classify_change's documented additive/backward-compatible
    // behavior. `touched_files` is condukt's own task field, so this is free.
    let sensitive = SensitiveConfig::default();
    match resolve_assessment(classify_change(
        &crate::schedule::task_action_text(task),
        &task.touched_files,
        "",
        &sensitive,
    )) {
        Resolved::Assessed(a) => GatherOutcome::Assessed(a),
        Resolved::Undetermined(reason) => GatherOutcome::Undetermined(reason),
    }
}

/// What [`resolve_assessment`] answers: a measured [`RiskAssessment`], or the
/// reason a classification could not be determined. Deliberately its own
/// two-arm type rather than `Result<RiskAssessment, String>`: `Result` would
/// hand a later reader `.ok()` — the exact erasure this fix removes from
/// [`resolve_assessment`] itself — so a `Resolved` cannot be flattened back to
/// `Option` by a one-line follow-up edit; a caller must match both arms again.
enum Resolved {
    /// The check ran and produced a measured value.
    Assessed(RiskAssessment),
    /// The check could not be run to a conclusion; carries why.
    Undetermined(String),
}

/// Resolve a risk classification's [`Determination`] into [`Resolved`]:
/// `Known(a)` → `Resolved::Assessed(a)`, `Undetermined(why)` →
/// `Resolved::Undetermined(why)` (the reason kept, not dropped), which
/// [`gather_assessment`]/[`run_gate_check`] degrade to [`GateExec::Escalate`]
/// — unchanged from before this fix.
///
/// Extracted as a pure function so this FAIL-CLOSED arm is directly testable:
/// [`gather_assessment`] builds its own `SensitiveConfig::default()`, which
/// always compiles, so the undetermined branch cannot be reached by feeding
/// `gather_assessment` a decomposition — only by a misconfigured glob list.
/// `require()` is `Determination`'s only extractor, returning
/// [`Required`] (not `std::Result`, which would reintroduce `.ok()` /
/// `unwrap_or*`); both `Required` arms are matched explicitly here, so there
/// is genuinely no `unwrap_or`-style path that could substitute a permissive
/// Low/reversible assessment for a risk that was never measured — the
/// docstring now matches what this function actually does, no `.ok()`
/// anywhere in the chain.
fn resolve_assessment(d: Determination<RiskAssessment>) -> Resolved {
    match d.require() {
        Required::Determined(a) => Resolved::Assessed(a),
        Required::Blocked(verdict) => {
            let reason = verdict
                .reason()
                .map(|r| r.as_str().to_string())
                .unwrap_or_else(|| "risk classification could not be determined".to_string());
            Resolved::Undetermined(reason)
        }
    }
}

/// Decide the [`GateExec`] verdict for one gathered outcome, plus the signals
/// to surface: `(risk, reversible)` when an assessment was measured (`None`
/// otherwise), and the classification's reason when — and only when — it
/// could not be determined. Pure and deterministic: the same `outcome` and
/// `policy_is_auto` always yield the same tuple.
///
/// Fail-closed: both "nothing to assess" ([`GatherOutcome::Missing`]) and
/// "could not be determined" ([`GatherOutcome::Undetermined`]) degrade to
/// [`GateExec::Escalate`] — the restricted side, unchanged by this fix. Only
/// `Undetermined` carries a reason, which the old `.ok()`-based
/// `resolve_assessment` used to drop on the floor by flattening it into the
/// same `None` as `Missing`; this keeps it instead of erasing it.
fn decide_from_outcome(
    outcome: &GatherOutcome,
    policy_is_auto: bool,
) -> (GateExec, Option<&RiskAssessment>, Option<&str>) {
    match outcome {
        GatherOutcome::Assessed(a) => (
            decide_gate_exec(a.risk, a.reversible, policy_is_auto),
            Some(a),
            None,
        ),
        GatherOutcome::Missing => (GateExec::Escalate, None, None),
        GatherOutcome::Undetermined(reason) => (GateExec::Escalate, None, Some(reason.as_str())),
    }
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
    let outcome = gather_assessment(cfg, cwd, run_id, task_id);
    let policy_is_auto = crate::policy_is_autonomous(cfg);

    let (verdict, assessment, undetermined_reason) = decide_from_outcome(&outcome, policy_is_auto);
    let verdict_str = match verdict {
        GateExec::AutoExec => "auto_exec",
        GateExec::Escalate => "escalate",
    };
    let risk = assessment.map(|a| risk_slug(a.risk).to_string());
    let reversible = assessment.map(|a| a.reversible);

    // Observable stdout JSON (verdict + every gathered signal + the task id).
    // `undetermined_reason` is `null` unless the classification itself could
    // not be determined, in which case it carries the `Verdict::Undetermined`
    // reason that used to be silently dropped by `.ok()`.
    let out = serde_json::json!({
        "verdict": verdict_str,
        "risk": risk,
        "reversible": reversible,
        "policy_is_auto": policy_is_auto,
        "task": task_id,
        "undetermined_reason": undetermined_reason,
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
        let summary = match undetermined_reason {
            // A classification that could not be determined gets its own
            // summary shape carrying the reason — previously silently
            // erased by the `.ok()` this fix removes.
            Some(reason) => format!(
                "gate-check escalated: task {task_id} risk classification could not be \
                 determined ({reason}); policy_is_auto={policy_is_auto}"
            ),
            None => format!(
                "gate-check escalated: task {task_id} risk={} reversible={} policy_is_auto={}",
                risk.as_deref().unwrap_or("unknown"),
                reversible
                    .map(|b| b.to_string())
                    .unwrap_or_else(|| "unknown".to_string()),
                policy_is_auto,
            ),
        };
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

    exit_code_for(verdict)
}

/// The process exit code for a [`GateExec`] verdict: `0` for `AutoExec`,
/// non-zero (`1`) for `Escalate` — preserving the human stop so a caller can
/// `if ! condukt gate check --run RID --task T; then escalate; fi`.
fn exit_code_for(verdict: GateExec) -> i32 {
    match verdict {
        GateExec::AutoExec => 0,
        GateExec::Escalate => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use harness_core::verdict::Determination;

    #[test]
    fn undetermined_classification_never_auto_execs_and_keeps_its_reason() {
        // FAIL-CLOSED: a risk that could not be measured must NOT arrive as a
        // Low/reversible assessment (which, under an auto policy, would
        // auto-exec). It resolves to `Resolved::Undetermined`, not a value —
        // and, unlike the old `.ok()`-based erasure, the reason string
        // travels with it instead of being dropped.
        let d: Determination<RiskAssessment> =
            Determination::undetermined("invalid sensitive glob `[`");
        let resolved = resolve_assessment(d);
        let reason = match resolved {
            Resolved::Undetermined(reason) => reason,
            Resolved::Assessed(a) => {
                panic!("an undetermined classification must not yield an assessment; got {a:?}")
            }
        };
        assert!(
            reason.contains("invalid sensitive glob"),
            "the Determination's reason must survive resolve_assessment unchanged; got {reason:?}"
        );

        // The full outcome->decision pipeline: Undetermined resolves to
        // Escalate, even under a fully-auto policy, AND the reason reaches
        // the caller-visible tuple `decide_from_outcome` builds (previously
        // impossible: the old `Option<RiskAssessment>` shape had nowhere to
        // put it). Escalate also maps to a non-zero exit code.
        let outcome = GatherOutcome::Undetermined(reason.clone());
        let (verdict, assessment, undetermined_reason) = decide_from_outcome(&outcome, true);
        assert_eq!(verdict, GateExec::Escalate);
        assert!(
            assessment.is_none(),
            "an undetermined outcome must not surface a fabricated assessment"
        );
        assert_eq!(
            undetermined_reason,
            Some(reason.as_str()),
            "the undetermined reason must reach the caller, not be erased"
        );
        assert_eq!(
            exit_code_for(verdict),
            1,
            "an undetermined classification must escalate with a non-zero exit code"
        );
    }

    #[test]
    fn missing_outcome_escalates_without_a_reason() {
        // Distinguish "nothing to assess" from "could not be determined":
        // Missing degrades to Escalate too, but carries no reason (there is
        // none to give — unlike Undetermined).
        let (verdict, assessment, undetermined_reason) =
            decide_from_outcome(&GatherOutcome::Missing, true);
        assert_eq!(verdict, GateExec::Escalate);
        assert!(assessment.is_none());
        assert_eq!(undetermined_reason, None);
        assert_eq!(exit_code_for(verdict), 1);
    }

    #[test]
    fn known_classification_is_passed_through_unchanged() {
        // Invariance guard: wrapping in a Determination must not alter what a
        // determinable classification decides.
        let a = RiskAssessment {
            risk: Risk::Low,
            reversible: true,
        };
        match resolve_assessment(Determination::Known(a.clone())) {
            Resolved::Assessed(resolved) => assert_eq!(resolved, a),
            Resolved::Undetermined(reason) => {
                panic!(
                    "a Known determination must resolve to Assessed; got Undetermined({reason:?})"
                )
            }
        }
        assert_eq!(
            decide_gate_exec(a.risk, a.reversible, true),
            GateExec::AutoExec
        );
    }

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
