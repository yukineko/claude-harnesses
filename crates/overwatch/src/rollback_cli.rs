/// CLI-facing glue for recording canary rollback events and AI-review findings
/// into the overwatch-readable stores that `review-queue` reads back.
///
/// Both record paths are **fail-soft**: `scripts/rollout-plugins.sh` calls
/// `record-rollback` while executing a rollback, and emission must NEVER break
/// the rollout. An unwritable store is swallowed (a warning to stderr) rather
/// than propagated, matching overwatch's observational/never-break-a-turn
/// invariant.
use crate::review_finding::{AuditVerdict, ReviewFinding};
use crate::rollback::{RollbackEvent, RollbackReason};
use crate::store;
use anyhow::Result;

/// Record one canary rollback event. Called by the rollout script when the
/// health gate advises/executes a rollback for a plugin.
///
/// Fail-soft: a store write error is reported to stderr but returns `Ok(())` so
/// the caller (the rollout) is never broken by logging.
#[allow(clippy::too_many_arguments)]
pub fn record(
    plugin: &str,
    from_version: Option<&str>,
    to_version: &str,
    stage: usize,
    reason: &str,
    detail: Option<&str>,
) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let now = store::now();
    let event = RollbackEvent::new(
        plugin.to_string(),
        from_version.map(str::to_string),
        to_version.to_string(),
        stage,
        RollbackReason::parse_lenient(reason),
        now,
        detail.map(str::to_string),
    );

    match store::append_rollback(&cwd, &event) {
        Ok(()) => {
            println!(
                "{}",
                serde_json::json!({ "recorded": true, "plugin": plugin, "stage": stage })
            );
        }
        Err(e) => {
            // Fail-soft: never break a rollout because the audit log couldn't
            // be written. Report and continue.
            eprintln!("overwatch: WARNING could not record rollback event (continuing): {e}");
            println!(
                "{}",
                serde_json::json!({ "recorded": false, "reason": "store-write-failed" })
            );
        }
    }
    Ok(())
}

/// Record one AI-review finding into the overwatch-readable findings store.
/// This is the defined ingestion point for the Continuous-Audit loop (and for
/// this crate's integration test). Fail-soft like `record`.
///
/// `verdict` is the adversarial verifier's tri-state result:
/// * `Some(raw)` — parsed by [`AuditVerdict::parse`]; anything unrecognized
///   becomes `Unverified` (undetermined resolves restrictively), never a
///   silent `Confirmed` and never a rejected/dropped record.
/// * `None` — the caller stated no verdict. Reads as `Confirmed`, preserving
///   the pre-tri-state ingestion contract in which `record-finding` was called
///   ONLY for the verifier's CONFIRMED subset (`scripts/continuous-audit.sh`).
///   That direction is also the LOUD one: the finding stays actionable on the
///   review surface instead of being quietly parked. Callers that mean
///   "undetermined" must say so with `--verdict unverified`.
#[allow(clippy::too_many_arguments)]
pub fn record_finding(
    finding_id: &str,
    source: &str,
    severity: Option<&str>,
    summary: &str,
    file: Option<&str>,
    rationale: Option<&str>,
    verdict: Option<&str>,
) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let now = store::now();
    let verdict = verdict
        .map(AuditVerdict::parse)
        .unwrap_or(AuditVerdict::Confirmed);
    let finding = ReviewFinding::new(
        finding_id.to_string(),
        source.to_string(),
        severity.map(str::to_string),
        summary.to_string(),
        file.map(str::to_string),
        rationale.map(str::to_string),
        now,
    )
    .with_verdict(verdict);

    match store::append_review_finding(&cwd, &finding) {
        Ok(()) => {
            println!(
                "{}",
                serde_json::json!({
                    "recorded": true,
                    "finding_id": finding_id,
                    "verdict": verdict.label(),
                })
            );
        }
        Err(e) => {
            eprintln!("overwatch: WARNING could not record review finding (continuing): {e}");
            println!(
                "{}",
                serde_json::json!({ "recorded": false, "reason": "store-write-failed" })
            );
        }
    }
    Ok(())
}
