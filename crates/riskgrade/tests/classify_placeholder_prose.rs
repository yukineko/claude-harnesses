// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! classify() over task prose that only looks like a redirect. Moved verbatim
//! from crates/blastguard/src/detect/tests.rs when the classifier moved to
//! riskgrade (2026-11-17); only the crate path changed:
//!   * the classify() half of `non_ascii_angle_bracket_placeholder_is_not_a_redirect`
//!     (its detect() half stays in blastguard), as the test below of the same name;
//!   * `benign_lookalike_task_prose_is_low_and_reversible`, whole.

#[test]
fn non_ascii_angle_bracket_placeholder_is_not_a_redirect() {
    // Regression for the 2026-07-17 false-gate incident: a condukt
    // done_criteria string with a Japanese placeholder (`<同一key>`)
    // followed by prose (`...`) was misread as a truncating redirect
    // whose target was the ellipsis, force-gating an ordinary task as
    // High/irreversible.
    let a = riskgrade::classify::classify(
        "2セッション(プロセス)がほぼ同時に同一keyをbeginする結合テストが追加され、\
         片方が必ずskip(exit 1、または明示的な待機後の順序どおりの成功)になり\
         両方が成功することは無いと固定される。<同一key>...を検証する",
    );
    assert_eq!(a.risk, riskgrade::classify::Risk::Low);
    assert!(a.reversible);
    assert!(!a.requires_gate());
}

#[test]
fn benign_lookalike_task_prose_is_low_and_reversible() {
    // The end-to-end path the bug broke: classify() over a task's prose.
    let a = riskgrade::classify::classify("implement fn parse() -> Result and pass --flag <value>");
    assert_eq!(a.risk, riskgrade::classify::Risk::Low);
    assert!(a.reversible);
    assert!(!a.requires_gate());
}
