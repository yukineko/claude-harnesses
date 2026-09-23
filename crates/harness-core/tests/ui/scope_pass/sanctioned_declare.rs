//! Positive control for `ScopeDeclaration` (t3 done_criteria item 3, pass 1).
//!
//! The point of `ScopeDeclaration` is that an *unresolved* scope must be
//! unrepresentable: every field is private, there is no public constructor, no
//! `Default`, no `new`, and no `From<ScopeDraft>`, so the only way to hold one
//! is to go through `ScopeDraft::declare`, which returns a `Determination` and
//! answers `Undetermined` for every unresolved draft.
//!
//! A contract that seals fabrication is only half the job — it must not also
//! seal the *sanctioned* path, or callers get pushed back into stringly-typed
//! workarounds (exactly the failure mode `tests/ui/verdict_pass/` exists to
//! catch for `Verdict`). This fixture is that other half: a fully-populated
//! draft must declare, `require()` must hand the declaration over, and the
//! accessors must be readable.
//!
//! **This fixture is expected to FAIL TO COMPILE until `ScopeDraft` /
//! `ScopeDeclaration` exist** in `harness_core::interrogate` per the API
//! contract (`.scratch/t3-scope-api-contract.md`, §1). That is the intended RED:
//! the type is absent, so the sanctioned path cannot be written. After the
//! implementation lands, this compiles and runs — a genuine Fail→Pass.
//!
//! What this fixture does NOT prove:
//! - It does not prove fabrication is impossible. Proving that needs the
//!   *negative* controls under `tests/ui/scope/` (a `pub` field, a `Default`, a
//!   `new`, a `From<ScopeDraft>`), which are deliberately NOT written yet: while
//!   the type is absent, a compile-fail fixture would "pass" on an unresolved
//!   import (E0432/E0433) rather than on the private-field error it is meant to
//!   pin, which proves nothing. Those land in pass 2, after the type exists.
//! - It does not cover the full `Undetermined` rule set from the contract. Rule
//!   1 (`write_paths: None`) is asserted below, but rules 2 (`write_paths:
//!   Some(vec![])`), 3 (`read_paths: None`), and 4 (empty/whitespace-only
//!   entries) are NOT exercised here. Those are ordinary runtime behaviors and
//!   belong in a normal `#[test]` module, not in a trybuild fixture; this file's
//!   job is the type-level contract.
//! - It does not check the *wording* of any `Undetermined` reason, only that the
//!   blocked arm is reachable and carries a fail-closed `Verdict`.
//! - Compiling proves the sanctioned path exists; it does not prove `declare`
//!   is pure. `interrogate.rs`'s IO-freedom is guarded separately and lexically
//!   by `tests/interrogate_purity.rs`.

use harness_core::interrogate::{ScopeDeclaration, ScopeDraft};
use harness_core::verdict::{Determination, Required};

/// The sanctioned consumption shape: `require()` forces both arms to be
/// written, and the blocked arm hands back an already-fail-closed `Verdict`
/// instead of a permissive empty declaration.
fn resolve(draft: ScopeDraft) -> Result<ScopeDeclaration, String> {
    let determination: Determination<ScopeDeclaration> = draft.declare();
    match determination.require() {
        Required::Determined(decl) => Ok(decl),
        // Fail closed: no empty/default declaration is substituted here.
        Required::Blocked(verdict) => Err(format!("{verdict:?}")),
    }
}

fn main() {
    // A fully-answered draft. `read_paths: Some(vec![])` is deliberately the
    // empty-but-answered case: "reads nothing" is a determined answer, unlike
    // "writes nothing" (contract §1).
    let draft = ScopeDraft {
        write_paths: Some(vec![
            "crates/hypothesis/src/hypothesis.rs".to_string(),
            "crates/hypothesis/src/store.rs".to_string(),
        ]),
        read_paths: Some(vec![]),
    };

    let decl = resolve(draft).expect("a fully-answered draft must declare");

    // The accessors must be readable — a declaration nobody can read would
    // satisfy the negative controls while being useless.
    assert_eq!(
        decl.write_paths(),
        [
            "crates/hypothesis/src/hypothesis.rs".to_string(),
            "crates/hypothesis/src/store.rs".to_string(),
        ]
    );
    assert!(
        decl.read_paths().is_empty(),
        "read_paths was answered as empty, and that answer must survive declaration"
    );

    // Rule 1 from the contract: a draft whose write_paths question was never
    // asked is Undetermined, NOT a permissive empty declaration. Asserted here
    // so this fixture has discriminating power beyond merely compiling.
    let never_asked = ScopeDraft {
        write_paths: None,
        read_paths: Some(vec![]),
    };
    assert!(
        resolve(never_asked).is_err(),
        "an unasked write_paths question must resolve to Undetermined, never to \
         an empty declaration"
    );

    // `Default` on the *draft* is sanctioned (an unresolved draft is a real
    // value); `Default` on the *declaration* is what must not exist. A default
    // draft has both questions unasked, so it must not declare.
    let empty_draft = ScopeDraft::default();
    assert!(
        resolve(empty_draft).is_err(),
        "a default (fully unanswered) draft must not yield a declaration"
    );
}
