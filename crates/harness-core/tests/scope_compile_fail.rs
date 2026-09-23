//! Compile-time contract tests for `ScopeDeclaration` (t3 done_criteria item 3).
//!
//! done_criteria item 3 requires that "scope (write_paths/read_paths) が未確定の
//! とき `ScopeDeclaration` 相当を発行できないことが型で表現され、コンパイル時に
//! 強制される" — i.e. an unresolved scope must be *unrepresentable*, not merely
//! rejected at runtime. Like `verdict_compile_fail.rs`, this is a *type-contract*
//! test rather than a behavior test: the claim is about what external code
//! cannot write, so the assertion has to be made to the compiler.
//!
//! # Pass 1 (this commit): positive control only
//!
//! Only `tests/ui/scope_pass/*.rs` is checked here. The negative controls —
//! `t.compile_fail("tests/ui/scope/*.rs")` over fixtures that try to fabricate a
//! `ScopeDeclaration` (a `pub` field, an `impl Default`, a `new`, a
//! `From<ScopeDraft>`; see the API contract §2) — are **deliberately absent**,
//! not forgotten, and are added in pass 2 once the types exist.
//!
//! The reason is an ordering hazard specific to compile-fail testing: while
//! `ScopeDraft`/`ScopeDeclaration` do not exist, a fixture that tries to forge
//! one fails to compile with an *unresolved import* (E0432/E0433) — so
//! `compile_fail` would report PASS for a reason that has nothing to do with the
//! contract being tested. A test that passes for the wrong reason proves nothing
//! (CLAUDE.md §2(b)), and committing its `.stderr` would freeze that wrong
//! reason in place as if it were the specification. So the negative controls wait
//! until the private-field error they are meant to pin is the error that actually
//! occurs.
//!
//! Pass 2 must therefore, after generating snapshots with `TRYBUILD=overwrite`,
//! **read the produced `.stderr` files and confirm the error codes are the
//! intended ones** (E0451 private field / E0599 no such associated item / E0277
//! unsatisfied trait) rather than E0432/E0433. `TRYBUILD=overwrite` bakes
//! whatever happened in as the expected answer, so an unreviewed `.stderr` is
//! self-fulfilling.
//!
//! # What this test does NOT prove (pass 1)
//!
//! - It does **not** prove that a `ScopeDeclaration` cannot be fabricated. That
//!   is exactly what the missing negative controls are for; today this harness
//!   only proves the sanctioned path is writable, so passing it says nothing
//!   about whether the seal exists.
//! - It does not prove `ScopeDraft::declare` implements the contract's four
//!   `Undetermined` rules; the fixture asserts rule 1 only (see its doc comment).
//! - It does not prove `declare` is pure. That invariant is guarded separately,
//!   and lexically, by `tests/interrogate_purity.rs`.
//!
//! Fragility note (applies once pass 2 lands): trybuild compares against
//! committed `.stderr` snapshots, which are rustc-version-sensitive — message
//! wording and error codes drift between releases. Regenerate with
//! `TRYBUILD=overwrite` after a toolchain bump, and re-check the error codes as
//! described above.

#[test]
fn scope_type_contract_positive_control() {
    let t = trybuild::TestCases::new();
    // Positive control: the sanctioned declare path MUST compile and run, so a
    // future seal is proven to reject only fabrication, not legitimate use.
    //
    // Pass 2 adds the negative controls here:
    //     t.compile_fail("tests/ui/scope/*.rs");
    // See this module's doc comment for why they are not present yet.
    t.pass("tests/ui/scope_pass/*.rs");
}
