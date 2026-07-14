/// CLI-facing glue for the Continuous-Audit round metrics ledger (2630b4c5).
///
/// `record` appends one round record to the audit-round ledger (the deterministic
/// counterpart of the LLM-driven finder/verifier step); `metrics` reads the
/// ledger back and prints the convergence report (per-round new-findings trend,
/// closure-rate, and a `converging` flag) either human-readable or as JSON.
///
/// Both paths are **fail-soft**: `scripts/continuous-audit.sh` calls `record`
/// while orchestrating a round, and a store-write failure must NEVER break the
/// audit loop — an unwritable ledger is reported to stderr rather than
/// propagated, matching overwatch's observational / never-break-a-turn invariant.
use crate::audit_round::{self, AuditRound, DEFAULT_CONVERGENCE_WINDOW};
use crate::review_finding::ReviewFinding;
use crate::store;
use anyhow::Result;

/// Record one Continuous-Audit round's metrics. Called by
/// `scripts/continuous-audit.sh` after a finder→verifier review round completes.
///
/// `target` is a comma/space-separated crate list (normalized internally).
/// Fail-soft: a store write error is reported to stderr but returns `Ok(())` so
/// the caller (the audit loop) is never broken by logging.
///
/// `finder_model` / `verifier_model` are optional. When BOTH are supplied and
/// denote the SAME model (canonical compare), the Continuous-Audit
/// `finder != verifier` MUST is violated (generation and verification share a
/// blind spot). This is enforced DETERMINISTICALLY here rather than left to
/// SKILL.md prose: a high-severity warning finding (deterministic, round-derived
/// id) is recorded into the review queue and a stderr warning is emitted. It is
/// **never** a hard failure — the audit loop is never broken (never-break-a-turn),
/// so a warning finding + the round record are preferred over aborting. When one
/// or both model args are omitted, the check is skipped entirely (backward
/// compatible: existing callers that pass no model args behave exactly as before).
pub fn record(
    round: String,
    target: &str,
    new_findings: u64,
    confirmed: u64,
    regression_tests_added: u64,
    finder_model: Option<&str>,
    verifier_model: Option<&str>,
) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let now = store::now();
    let targets = audit_round::parse_targets(target);
    let record = AuditRound::new(
        round,
        &targets,
        new_findings,
        confirmed,
        regression_tests_added,
        now,
    );

    match store::append_audit_round(&cwd, &record) {
        Ok(()) => {
            println!(
                "{}",
                serde_json::json!({
                    "recorded": true,
                    "round": record.round,
                    "targets": record.targets,
                    "new_findings": new_findings,
                    "confirmed": confirmed,
                    "regression_tests_added": regression_tests_added,
                })
            );
        }
        Err(e) => {
            eprintln!("overwatch: WARNING could not record audit round (continuing): {e}");
            println!(
                "{}",
                serde_json::json!({ "recorded": false, "reason": "store-write-failed" })
            );
        }
    }

    // Deterministic finder != verifier model-diversity enforcement. Runs AFTER
    // (and independently of) the round append so it fires even if the ledger
    // write failed. Only checks when BOTH models were supplied (backward compat).
    enforce_model_diversity(&cwd, &record.round, finder_model, verifier_model, now);

    Ok(())
}

/// Enforce the `finder != verifier` MUST for a recorded round. When both models
/// are present and canonically equal, record a high-severity warning finding
/// (round-derived id, idempotent) into the review queue and warn on stderr.
/// Fail-soft and non-fatal: a store-write failure is only logged; the audit loop
/// is never broken.
fn enforce_model_diversity(
    cwd: &std::path::Path,
    round: &str,
    finder_model: Option<&str>,
    verifier_model: Option<&str>,
    now: i64,
) {
    let (Some(finder), Some(verifier)) = (finder_model, verifier_model) else {
        return;
    };
    if !audit_round::same_model(finder, verifier) {
        return;
    }
    let finding_id = audit_round::model_collision_finding_id(round);
    let summary = format!(
        "finder と verifier が同一モデル: MUST 違反 (finder={finder}, verifier={verifier}) — \
         model diversity 要件 (生成と検証の盲点共有を防ぐ) を満たさない"
    );
    let finding = ReviewFinding::new(
        finding_id.clone(),
        "continuous-audit".to_string(),
        Some("high".to_string()),
        summary,
        None,
        None,
        now,
    );
    eprintln!(
        "overwatch: WARNING finder と verifier が同一モデル ({finder}): MUST 違反 — \
         review-queue に警告 finding ({finding_id}) を記録 (loop は継続 / fail-soft)"
    );
    if let Err(e) = store::append_review_finding(cwd, &finding) {
        eprintln!("overwatch: WARNING could not record model-collision finding (continuing): {e}");
    }
}

/// Close a Continuous-Audit round: SET its `regression_tests_added` to `tests`
/// (the fix-side feedback the finder-time [`record`] cannot know, since the
/// regression tests don't exist when a round is first recorded). Read-modify-write
/// on the ledger: read all rounds, update the one matching `round` in memory via
/// [`audit_round::set_round_tests`], write it back. After this, `audit-metrics`
/// reports the round's honest `closure_rate` and the cumulative `converging`
/// signal reflects the fixes actually landed.
///
/// Fail-soft (never-break-a-turn): an unknown round-id, or a store error, is
/// reported (JSON on stdout + stderr note) but never panics. Idempotent: SETting
/// the same `tests` twice is a no-op, so re-running a backfill does not
/// double-count.
pub fn close(round: String, tests: u64) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let rounds = store::read_audit_rounds(&cwd).unwrap_or_default();
    let (updated, found) = audit_round::set_round_tests(&rounds, &round, tests);
    if !found {
        eprintln!(
            "overwatch: WARNING audit-round close: no round matching id {round:?} (ledger unchanged)"
        );
        println!(
            "{}",
            serde_json::json!({ "closed": false, "reason": "round-not-found", "round": round })
        );
        return Ok(());
    }
    match store::rewrite_audit_rounds(&cwd, &updated) {
        Ok(()) => {
            println!(
                "{}",
                serde_json::json!({
                    "closed": true,
                    "round": round,
                    "regression_tests_added": tests,
                })
            );
        }
        Err(e) => {
            eprintln!("overwatch: WARNING could not rewrite audit rounds (continuing): {e}");
            println!(
                "{}",
                serde_json::json!({ "closed": false, "reason": "store-write-failed", "round": round })
            );
        }
    }
    Ok(())
}

/// Read the audit-round ledger and print convergence metrics. `window` bounds
/// how many trailing rounds the `converging` check considers (default
/// [`DEFAULT_CONVERGENCE_WINDOW`]). Fail-soft: a missing/empty ledger yields a
/// zero-round report rather than an error.
pub fn metrics(json: bool, window: Option<usize>) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let rounds = store::read_audit_rounds(&cwd).unwrap_or_default();
    let window = window.unwrap_or(DEFAULT_CONVERGENCE_WINDOW);
    let report = audit_round::compute_metrics(&rounds, window);

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    if report.rounds.is_empty() {
        println!("(no audit rounds recorded yet — run scripts/continuous-audit.sh)");
        return Ok(());
    }

    println!("Continuous-Audit convergence report");
    println!("  rounds recorded: {}", report.rounds.len());
    println!("  per-round new-findings (decreasing => converging):");
    for r in &report.rounds {
        let cr = r
            .closure_rate
            .map(|v| format!("{:.2}", v))
            .unwrap_or_else(|| "n/a".to_string());
        println!(
            "    round {:>3}: new={:<4} confirmed={:<4} tests={:<4} closure={}",
            r.round, r.new_findings, r.confirmed, r.regression_tests_added, cr
        );
    }
    println!("  total new findings:       {}", report.total_new_findings);
    println!(
        "  cumulative confirmed:     {}",
        report.cumulative_confirmed
    );
    println!(
        "  cumulative reg. tests:    {}",
        report.cumulative_regression_tests_added
    );
    let overall = report
        .closure_rate
        .map(|v| format!("{:.2}", v))
        .unwrap_or_else(|| "n/a".to_string());
    println!("  overall closure-rate:     {overall}");
    println!(
        "  converging (last {} rounds): {}",
        report.convergence_window, report.converging
    );
    Ok(())
}
