/// CLI-facing glue for recording canary rollback events and AI-review findings
/// into the overwatch-readable stores that `review-queue` reads back.
///
/// Both record paths are **fail-soft**: `scripts/rollout-plugins.sh` calls
/// `record-rollback` while executing a rollback, and emission must NEVER break
/// the rollout. An unwritable store is swallowed (a warning to stderr) rather
/// than propagated, matching overwatch's observational/never-break-a-turn
/// invariant.
use crate::review_finding::{adjudicate, read_probe, AuditVerdict, ReviewFinding, SignOff};
use crate::rollback::{RollbackEvent, RollbackReason};
use crate::store;
use anyhow::Result;
use std::path::Path;

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
/// this crate's integration test). A store-write failure is reported (stderr +
/// `"recorded": false`) and returns `Ok(())`, like `record`.
///
/// `verdict` is the verifier's ASSERTED tri-state result, parsed by
/// [`AuditVerdict::parse`] (anything unrecognized becomes `Unverified`). The
/// asserted verdict is then passed through [`adjudicate`] together with the
/// optional probe result file (`probe`) and human sign-off (`signed_off_by`),
/// and only the adjudicated verdict is stored (backlog 80a46e9f): a REFUTED
/// without BOTH a `not_reproduced` probe and a sign-off is stored as
/// `Unverified`; a `reproduced` probe turns a REFUTED into `Confirmed`. When
/// the stored verdict differs from the asserted one, the reason — naming the
/// missing piece — is printed on stderr and the asserted verdict is echoed in
/// the stdout JSON as `asserted`.
#[allow(clippy::too_many_arguments)]
pub fn record_finding(
    finding_id: &str,
    source: &str,
    severity: Option<&str>,
    summary: &str,
    file: Option<&str>,
    rationale: Option<&str>,
    verdict: &str,
    probe: Option<&Path>,
    signed_off_by: Option<&str>,
) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let now = store::now();
    // The caller is required to pass a verdict (no default is invented). An
    // UNRECOGNISED value resolves to `Unverified` inside `AuditVerdict::parse`
    // — undetermined to the restrictive side.
    let asserted = AuditVerdict::parse(verdict);
    let adjudication = adjudicate(
        asserted,
        probe.map(read_probe),
        &SignOff::from_flag(signed_off_by),
    );
    if let Some(note) = &adjudication.note {
        eprintln!("overwatch: record-finding {finding_id}: {note}");
    }
    let verdict = adjudication.stored;
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
                    "asserted": asserted.label(),
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
