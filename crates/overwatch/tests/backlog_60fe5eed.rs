#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! backlog 60fe5eed: `review-metrics` "median latency" is computed as
//! `disposition.resolved_ts - earliest finding ts` for EVERY disposition. An
//! `auto-reconcile(commit ..)` disposition is stamped with the time the
//! reconcile BATCH ran (reconcile.rs: `Disposition::new(.., now)`), not the
//! time the fix landed. So a finding fixed one day after it was raised, but
//! reconciled in a batch 70 days later, is reported as a 70-day latency — the
//! live store printed `median latency (secs): 6055122` (~70 days) with all 19
//! dispositions auto-reconcile stamped at the same instant.

use overwatch::disposition::{median_latency_secs, Disposition, DispositionVerdict};
use overwatch::review_finding::ReviewFinding;

const DAY: i64 = 86_400;

#[test]
#[ignore = "backlog 60fe5eed: open defect, remove ignore when fixed"]
fn auto_reconcile_batch_time_is_not_reported_as_time_to_fix() {
    let raised = 1_000_000;
    // The fix commit landed one day after the finding; reconcile-fixed only
    // ran 70 days later and stamped `now` as resolved_ts.
    let batch_ran = raised + 70 * DAY;
    let findings = vec![ReviewFinding::new(
        "CA-overwatch-001".to_string(),
        "continuous-audit".to_string(),
        Some("high".to_string()),
        "s".to_string(),
        None,
        None,
        raised,
    )];
    let dispositions = vec![Disposition::new(
        "CA-overwatch-001".to_string(),
        DispositionVerdict::Confirmed,
        "auto-reconcile(commit abc1234)".to_string(),
        batch_ran,
    )];
    let median = median_latency_secs(&dispositions, &findings);
    assert_ne!(
        median,
        Some(70 * DAY),
        "median latency reports the auto-reconcile batch time (70 days) as the \
         time-to-fix; the fix landed after 1 day"
    );
}
