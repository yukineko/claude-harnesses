// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Compile-time contract tests, from `blastguard`'s vantage point, for
//! `harness_core::verdict`.
//!
//! `blastguard::diffrisk` migrated onto `harness_core::verdict::Determination`
//! (see `SensitiveConfig::any_sensitive`). This suite pins that the same
//! type-level guarantees `harness-core` proves for itself
//! (`harness-core/tests/verdict_compile_fail.rs`) also hold when the crate is
//! consumed as a downstream dependency, not just from within `harness-core`
//! itself: an outside crate still cannot fabricate a `Clean` verdict, and the
//! sanctioned Clean-minting / `Determination`-resolving paths blastguard
//! actually uses still compile.
//!
//! Each fixture under `tests/ui/verdict/` MUST fail to compile, and for the
//! *intended* reason — the committed `.stderr` files record which error. The
//! positive control under `tests/ui/verdict_pass/` MUST compile, proving the
//! contract does not over-block the sanctioned paths this crate relies on.
//!
//! Fragility note: trybuild compares against committed `.stderr` snapshots,
//! which are rustc-version-sensitive (message wording/error codes drift
//! between releases). Regenerate with `TRYBUILD=overwrite` after a toolchain
//! bump. The snapshots in this tree were generated against rustc 1.97.1
//! (8bab26f4f 2026-07-14).

#[test]
fn verdict_type_contract_compile_fail_from_blastguard() {
    let t = trybuild::TestCases::new();
    // Negative controls: every one of these MUST be rejected by the compiler.
    t.compile_fail("tests/ui/verdict/*.rs");
    // Positive control: the sanctioned paths MUST still compile, so the
    // contract is proven to reject only fabrication, not legitimate use.
    t.pass("tests/ui/verdict_pass/*.rs");
}
