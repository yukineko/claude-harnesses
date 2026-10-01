// 丸ごと integration test なので unwrap/expect/panic を許可する。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Fault-injection fail-closed check for overwatch's Continuous-Audit
//! convergence verdict (backlog 8696dd7e, slice 2: first gate-crate adopter).
//!
//! `store::read_audit_rounds` -> `compute_metrics(..).converging` is the verdict
//! path behind `overwatch audit-metrics`. This test forces EVERY boundary IO
//! entry to `Undetermined` (`FaultPlan::blind()`, via
//! `degrade::assert_fails_closed`) and requires that the verdict does not stay
//! permissive. The ledger is deliberately a genuinely converging history
//! (`Some(true)` when read), so a gate that ignores the fault and reads it
//! anyway is observably permissive, not vacuously restrictive.
//!
//! Needs `harness-core = { path = "../harness-core", features =
//! ["fault-injection"] }` under overwatch's `[dev-dependencies]`; without it
//! `degrade::assert_fails_closed` does not exist and this file fails to compile.

use harness_core::degrade::assert_fails_closed;
use harness_core::verdict::Required;
use overwatch::audit_round::{self, AuditRound, DEFAULT_CONVERGENCE_WINDOW};
use overwatch::store;

/// The gate under test: read the persisted ledger, report `converging`.
/// `None` = could not reach a verdict (restrictive); `Some(true)` = permissive.
fn converging_verdict(cwd: &std::path::Path) -> Option<bool> {
    let read = store::read_audit_rounds(cwd).ok()?;
    let rounds = match read.require() {
        Required::Determined(rounds) => rounds,
        Required::Blocked(_) => return None,
    };
    audit_round::compute_metrics(&rounds, DEFAULT_CONVERGENCE_WINDOW).converging
}

#[test]
fn blind_boundary_never_leaves_the_convergence_verdict_permissive() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    // A plain `.git` directory: storage_root resolves without any boundary IO.
    std::fs::create_dir(repo.path().join(".git")).unwrap();
    std::env::set_var("HOME", home.path());

    // Non-increasing new-findings => converging when read.
    for (i, n) in [9u64, 5, 2, 1].iter().enumerate() {
        let round = AuditRound::new(
            format!("r{i}"),
            &["overwatch".to_string()],
            *n,
            0,
            0,
            1_700_000_000 + i as i64,
        );
        store::append_audit_round(repo.path(), &round).unwrap();
    }

    // Anti-vacuity control: with no fault the gate really is permissive.
    assert_eq!(
        converging_verdict(repo.path()),
        Some(true),
        "control: an intact converging ledger must read as converging"
    );

    assert_fails_closed(|| converging_verdict(repo.path()));
}
