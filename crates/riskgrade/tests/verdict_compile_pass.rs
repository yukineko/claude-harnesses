// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Compile-time positive control, from riskgrade's vantage point, for
//! `harness_core::verdict`: the `Determination` path diffrisk uses must still
//! compile and run. The negative fixtures (fabricating a `Clean`, a permissive
//! extractor) stay in blastguard's and harness-core's suites; this crate adds
//! only the diffrisk path that moved here from blastguard on 2026-11-17.

#[test]
fn verdict_type_contract_pass_from_riskgrade() {
    let t = trybuild::TestCases::new();
    t.pass("tests/ui/verdict_pass/*.rs");
}
