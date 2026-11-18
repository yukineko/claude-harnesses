// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The `classify (lib)` and `9aced690` sections of
//! crates/blastguard/tests/backlog_audit_s02.rs, moved verbatim with the
//! classifier (blastguard::classify -> riskgrade::classify, 2026-11-17). Only
//! the crate path changed; the six tests, and the #[ignore] markers on the five
//! open defects, are unchanged.

// ---------------------------------------------------------------- classify (lib)
#[test]
#[ignore = "backlog 1d59cf7f: open defect, remove ignore when fixed"]
fn backlog_1d59cf7f_editing_the_rollout_script_file_is_not_a_deploy() {
    // Control: a real deploy verb still gates.
    assert!(riskgrade::classify::classify("git push --force origin main").requires_gate());
    let a = riskgrade::classify::classify(
        "fix a typo in scripts/rollout-plugins.sh (edit the file only)",
    );
    assert!(!a.requires_gate(), "filename-only mention gated: {a:?}");
}

#[test]
#[ignore = "backlog 2b1065a1: open defect, remove ignore when fixed"]
fn backlog_2b1065a1_negated_deploy_word_does_not_force_gate() {
    let a = riskgrade::classify::classify(
        "do NOT run the rollout in this task; distribution belongs to t3",
    );
    assert!(!a.requires_gate(), "negated deploy word gated: {a:?}");
}

#[test]
#[ignore = "backlog 90e78490: open defect, remove ignore when fixed"]
fn backlog_90e78490_comparison_operators_in_prose_do_not_force_gate() {
    let a = riskgrade::classify::classify("done when last >= first");
    assert!(!a.requires_gate(), "'>=' in prose gated: {a:?}");
    let b = riskgrade::classify::classify("Known(m) and m > built_at is not fresh");
    assert!(!b.requires_gate(), "'>' in prose gated: {b:?}");
    // Control: a real truncating redirect to a system file still gates.
    assert!(riskgrade::classify::classify("cargo test > /etc/hosts").requires_gate());
}

#[test]
#[ignore = "backlog aaff04c5: open defect, remove ignore when fixed"]
fn backlog_aaff04c5_fat_arrow_in_prose_does_not_force_gate() {
    let a = riskgrade::classify::classify("map each key => its value");
    assert!(!a.requires_gate(), "'=>' in prose gated: {a:?}");
}

#[test]
#[ignore = "backlog 126d038a: open defect, remove ignore when fixed"]
fn backlog_126d038a_bare_gt_in_prose_does_not_force_gate() {
    let a = riskgrade::classify::classify("Known(m) and m > built_at is not fresh");
    assert!(!a.requires_gate(), "'>' in prose gated: {a:?}");
}

// ---------------------------------------------------------------- 9aced690
/// Kills the `&&` -> `||` mutant in `RiskAssessment::requires_gate`
/// (classify.rs:44): the predicate is "High AND not reversible", so each half
/// alone must NOT gate. NOT ignored: this is a regression test that passes.
#[test]
fn backlog_9aced690_requires_gate_needs_both_high_and_irreversible() {
    use riskgrade::classify::{Risk, RiskAssessment};
    let both = RiskAssessment {
        risk: Risk::High,
        reversible: false,
    };
    let high_only = RiskAssessment {
        risk: Risk::High,
        reversible: true,
    };
    let irreversible_only = RiskAssessment {
        risk: Risk::Medium,
        reversible: false,
    };
    let neither = RiskAssessment {
        risk: Risk::Low,
        reversible: true,
    };
    assert!(both.requires_gate());
    assert!(
        !high_only.requires_gate(),
        "High + reversible must not gate"
    );
    assert!(
        !irreversible_only.requires_gate(),
        "Medium + irreversible must not gate"
    );
    assert!(!neither.requires_gate());
}
