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
/// The split now covers *loading* as well as *classification* (backlog
/// `6d451627`, previously recorded here as a KNOWN GAP). `Missing` used to
/// merge "there is genuinely nothing to assess" with "there is something, but
/// it could not be read", because [`state::load_decomposition`] returns a plain
/// `Result` over `std::fs::read_to_string` and ENOENT was indistinguishable
/// from a read failure by the time it arrived. Neither was a fail-open — every
/// path escalates — but a load failure was reported with a `null`
/// `undetermined_reason`, i.e. as if nothing had needed assessing.
/// [`gather_assessment`] now reads through
/// [`state::load_decomposition_determined`], so absence stays `Missing` and a
/// read failure (as well as an unparseable decomposition) becomes
/// `Undetermined` with the reason.
#[derive(Debug)]
enum GatherOutcome {
    /// Nothing to assess, as a real observation: no such decomposition
    /// (ENOENT), or a decomposition that holds no task with this id.
    Missing,
    /// A risk was measured.
    Assessed(RiskAssessment),
    /// The assessment could not be determined; carries why. Reached by a
    /// decomposition that exists but could not be read, one that could not be
    /// parsed, or a risk classification that could not be measured.
    Undetermined(String),
}

/// Gather the `{risk, reversible}` signals for one gated task, FAIL-CLOSED, in
/// three answers: a decomposition that does not exist, or that holds no task
/// with this id, yields [`GatherOutcome::Missing`]; a decomposition that
/// exists but could not be read or parsed, or a risk classification that could
/// not be measured, yields [`GatherOutcome::Undetermined`] with the reason.
/// Both degrade to [`GateExec::Escalate`] in the caller — the restricted side,
/// unchanged — but only the second reaches the operator with a reason
/// attached. Never panics.
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
    let raw = match state::load_decomposition_determined(cfg, cwd, run_id) {
        // Genuinely no such decomposition: a real observation of absence.
        Determination::Known(None) => return GatherOutcome::Missing,
        Determination::Known(Some(raw)) => raw,
        // It is there and could not be read. Saying "nothing to assess" here
        // would report an unexamined gate as an empty one.
        Determination::Undetermined(why) => {
            return GatherOutcome::Undetermined(why.as_str().to_string())
        }
    };
    let dec = match serde_json::from_str::<crate::model::Decomposition>(&raw) {
        Ok(dec) => dec,
        // Present, readable, and not parseable. The bytes exist, so this is a
        // failure to understand them, not an absence of them.
        Err(e) => {
            return GatherOutcome::Undetermined(format!(
                "the decomposition for run '{run_id}' could not be parsed: {e}"
            ))
        }
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
    use std::path::PathBuf;

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

    /// A `Config` whose state lives under `tmp`. Only the fields
    /// `gather_assessment` can reach matter; the rest are inert defaults.
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

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "condukt-gate-exec-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The KNOWN GAP this closes (backlog `6d451627`): a decomposition that
    /// EXISTS but cannot be read used to arrive as
    /// [`GatherOutcome::Missing`] — the same answer as "there is no such run"
    /// — so `condukt gate check` reported a read failure with
    /// `undetermined_reason: null`, indistinguishable from "nothing needed
    /// assessing". Neither is a fail-open (both escalate), but the operator
    /// cannot tell an unexamined gate from an empty one, which is the exact
    /// conflation `harness_core::boundary` exists to prevent.
    #[cfg(unix)]
    #[test]
    fn an_unreadable_decomposition_is_undetermined_not_missing() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = temp_dir("unreadable-decomposition");
        let cfg = test_cfg(&tmp);
        let cwd = tmp.join("repo");
        std::fs::create_dir_all(&cwd).unwrap();

        let path = state::decomposition_path(&cfg, &cwd, "run-x");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"{\"goal\":\"g\",\"tasks\":[]}").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        // If chmod 000 does not actually deny this uid (root), the premise of
        // the test is absent — say so rather than asserting the wrong thing.
        let denied = std::fs::read_to_string(&path).is_err();

        let outcome = gather_assessment(&cfg, &cwd, "run-x", "t1");

        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644));
        let _ = std::fs::remove_dir_all(&tmp);

        assert!(
            denied,
            "precondition: chmod 000 must deny this uid (running as root?)"
        );
        let reason = match &outcome {
            GatherOutcome::Undetermined(reason) => reason.clone(),
            other => panic!(
                "a decomposition that exists but cannot be read must be Undetermined, not \
                 folded into the same answer as 'no such run': got {other:?}"
            ),
        };
        assert!(
            reason.contains("run-x") || reason.contains(&path.display().to_string()),
            "the reason must name what could not be read; got {reason:?}"
        );

        // ...and it must reach the caller-visible field that was `null` before.
        let (verdict, assessment, undetermined_reason) = decide_from_outcome(&outcome, true);
        assert_eq!(verdict, GateExec::Escalate);
        assert!(assessment.is_none());
        assert!(
            undetermined_reason.is_some(),
            "undetermined_reason must not be null for a read failure — that is the \
             reporting gap 6d451627 is about"
        );
    }

    /// Anti-vacuity control for the test above: a decomposition that genuinely
    /// does not exist must STAY [`GatherOutcome::Missing`]. Without this, a
    /// change that answered `Undetermined` for everything would pass the test
    /// above while destroying the distinction it exists to create.
    #[test]
    fn a_genuinely_absent_decomposition_stays_missing() {
        let tmp = temp_dir("absent-decomposition");
        let cfg = test_cfg(&tmp);
        let cwd = tmp.join("repo");
        std::fs::create_dir_all(&cwd).unwrap();

        let outcome = gather_assessment(&cfg, &cwd, "run-does-not-exist", "t1");
        let _ = std::fs::remove_dir_all(&tmp);

        assert!(
            matches!(outcome, GatherOutcome::Missing),
            "a run that was never created is a real observation of absence, not a \
             judgment failure: got {outcome:?}"
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
