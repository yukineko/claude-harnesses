//! Compile-time contract tests for `harness_core::verdict`.
//!
//! These are *type-contract* tests, not behavior tests: they assert, as compiler
//! errors, that external code **cannot** fabricate a pass or collapse the
//! "undetermined" answer into a permissive value. The prose in `verdict.rs`
//! claims a fail-open is unrepresentable; this test pins that claim to the
//! compiler so a future edit that reopens the hole fails CI instead of a review.
//!
//! Each fixture under `tests/ui/verdict/` MUST fail to compile, and for the
//! *intended* reason — the committed `.stderr` files record which error. The
//! positive control under `tests/ui/verdict_pass/` MUST compile, proving the
//! contract does not over-block the sanctioned Clean-minting paths.
//!
//! Fragility note: trybuild compares against committed `.stderr` snapshots, which
//! are rustc-version-sensitive (message wording/error codes drift between
//! releases). Regenerate with `TRYBUILD=overwrite` after a toolchain bump. The
//! snapshots in this tree were generated against rustc 1.97.1 (8bab26f4f
//! 2026-07-14).

#[test]
fn verdict_type_contract_compile_fail() {
    let t = trybuild::TestCases::new();
    // Negative controls: every one of these MUST be rejected by the compiler.
    t.compile_fail("tests/ui/verdict/*.rs");
    // Positive control: the sanctioned paths MUST still compile, so the contract
    // is proven to reject only fabrication, not legitimate use.
    t.pass("tests/ui/verdict_pass/*.rs");
}
