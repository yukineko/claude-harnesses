#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! backlog 7f07228e: the LIBRARY entry point `overwatch::store::record_finding`
//! takes no verdict and builds the row with `ReviewFinding::new`, which stamps
//! `AuditVerdict::Confirmed` unconditionally. Its producers (condukt gate_exec
//! escalations, specguard structural drift / shard flags, propguard outages)
//! record raw gate observations that no adversarial verifier ever checked, yet
//! they land as CONFIRMED and are therefore forwarded by
//! `review-queue --to-backlog` as actionable work. eda212a0 made the CLI state
//! its verdict; this path still cannot express UNVERIFIED at all.
//!
//! One test per binary: it swaps HOME for a temp dir, so it must not race
//! another test in the same process.

use overwatch::review_finding::AuditVerdict;

#[test]
#[ignore = "backlog 7f07228e: open defect, remove ignore when fixed"]
fn library_record_finding_does_not_stamp_an_unverified_gate_signal_as_confirmed() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    std::env::set_var("HOME", home.path());

    // Exactly what condukt's gate-exec escalate path records
    // (crates/condukt/src/gate_exec.rs): a raw "a human must decide" signal.
    overwatch::store::record_finding(
        project.path(),
        "gate-exec:run-1:t4".to_string(),
        "condukt-gate".to_string(),
        Some("high".to_string()),
        "gate-check escalated: task t4 risk=high reversible=false policy_is_auto=false".to_string(),
        None,
        None,
    )
    .expect("record_finding");

    let rows = overwatch::store::read_review_findings(project.path()).expect("read");
    assert_eq!(
        rows.len(),
        1,
        "precondition: the row was recorded: {rows:?}"
    );
    assert_ne!(
        rows[0].verdict,
        AuditVerdict::Confirmed,
        "a gate-exec escalation recorded through the library path is stored \
         CONFIRMED although nothing verified it: {:?}",
        rows[0]
    );
}
