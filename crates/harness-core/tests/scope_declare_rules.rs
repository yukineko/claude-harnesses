//! Runtime behavior tests for `ScopeDraft::declare` (t3, API contract §1).
//!
//! The type-level half of done_criteria item 3 — that a `ScopeDeclaration`
//! cannot be *fabricated* — is pinned by `scope_compile_fail.rs` and (in pass 2)
//! the negative controls under `tests/ui/scope/`. That half is not sufficient on
//! its own: a sealed type whose only constructor happily answers `Known` for an
//! empty `write_paths` is still a fail-open. The seal would guarantee that every
//! declaration came from `declare`, and `declare` would guarantee nothing.
//!
//! These tests are the other half: the four `Undetermined` rules the contract
//! fixes, plus the boundary positive that stops the trivial over-blocking
//! implementation, plus the accessor round-trip that stops the trivial
//! empty-accessor implementation.
//!
//! **Expected to FAIL TO COMPILE until `ScopeDraft` / `ScopeDeclaration` /
//! `declare` exist** in `harness_core::interrogate` per the contract. That is
//! the intended RED (E0432, unresolved import), and it is a genuine Fail→Pass:
//! unlike a trybuild `compile_fail` fixture — which would "pass" on that same
//! unresolved import and therefore prove nothing — an ordinary `#[test]` cannot
//! be satisfied by failing to compile. Nothing here can pass for the wrong
//! reason.
//!
//! Cross-cutting caveat, stated once here and referenced from each test below:
//! **tests 1–4 are jointly satisfiable by a `declare` that answers
//! `Undetermined` unconditionally.** They are negative cases; on their own they
//! reward maximal blocking. `declare_accepts_read_paths_answered_as_empty` and
//! `declared_scope_preserves_the_declared_paths` are the counterweight — they
//! fail for exactly that degenerate implementation. Neither group is meaningful
//! without the other.

use harness_core::interrogate::{ScopeDeclaration, ScopeDraft};
use harness_core::verdict::{Determination, Required};

/// Build a draft from optional string slices, so each test states only the shape
/// it cares about. `None` means "the question was never asked"; `Some(&[])`
/// means "asked and answered with nothing".
fn draft(write: Option<&[&str]>, read: Option<&[&str]>) -> ScopeDraft {
    fn own(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_string()).collect()
    }
    ScopeDraft {
        write_paths: write.map(own),
        read_paths: read.map(own),
    }
}

/// Assert that `declare` refused. Spells out the failure as the specific
/// fail-open being guarded against, so a future reader knows what a red here
/// means rather than just "assert failed".
fn assert_refused(d: Determination<ScopeDeclaration>, case: &str) {
    match d {
        Determination::Undetermined(_) => {}
        Determination::Known(decl) => panic!(
            "{case}: declare() must answer Undetermined, but answered \
             Known({decl:?}). An unresolved scope must not collapse into a \
             permissive declaration — \"could not determine\" is not \"clean\"."
        ),
    }
}

/// Contract §1 rule 1: `write_paths: None` — the question was never asked.
///
/// What this does NOT prove: it does not check the `Undetermined` reason string
/// at all (the contract requires the reason to name which rule fired; the
/// wording is untested here). It does not prove rule 1 is what fired — only
/// that *something* refused; `read_paths` is a valid non-empty answer in this
/// draft so no other rule has an input to trip on, but a `declare` that refuses
/// unconditionally also passes (see the module-level caveat).
#[test]
fn declare_refuses_when_write_paths_was_never_asked() {
    let d = draft(None, Some(&["crates/hypothesis/src/store.rs"])).declare();
    assert_refused(d, "rule 1 (write_paths: None)");
}

/// Contract §1 rule 2: `write_paths: Some(vec![])` — asked, and answered
/// "nothing". This is the rule that matters most: "there is nothing to write"
/// must not be recorded as a satisfied write surface. An empty set read as
/// "nothing wrong" is the exact fail-open shape CLAUDE.md §3 names.
///
/// What this does NOT prove: the reason wording is unchecked. It also does not
/// prove the asymmetry with `read_paths` — that empty *reads* are accepted while
/// empty *writes* are refused is only established together with
/// `declare_accepts_read_paths_answered_as_empty`; this test alone is equally
/// consistent with an implementation that refuses every empty set.
#[test]
fn declare_refuses_when_write_paths_is_answered_as_empty() {
    let d = draft(Some(&[]), Some(&["crates/hypothesis/src/store.rs"])).declare();
    assert_refused(d, "rule 2 (write_paths: Some(vec![]))");
}

/// Contract §1 rule 3: `read_paths: None` — the question was never asked.
/// Distinct from `Some(vec![])`, which rule 5 accepts.
///
/// What this does NOT prove: the reason wording is unchecked, and it does not
/// prove rule 3 specifically fired rather than some broader refusal.
#[test]
fn declare_refuses_when_read_paths_was_never_asked() {
    let d = draft(Some(&["crates/hypothesis/src/hypothesis.rs"]), None).declare();
    assert_refused(d, "rule 3 (read_paths: None)");
}

/// Contract §1 rule 4: any entry in either set is empty or whitespace-only. A
/// blank path is not a declared path; accepting one would let a caller satisfy
/// the write-surface requirement with a placeholder.
///
/// Every case keeps the *other* field a valid answer, so rules 1–3 cannot be
/// what fires.
///
/// What this does NOT prove: the reason wording is unchecked. It does not
/// enumerate every whitespace form (no `\r`, no non-breaking space, no unicode
/// whitespace) — it checks the empty string, spaces, and a tab. It also says
/// nothing about entries that are non-blank but meaningless (`"."`, `"nonexistent
/// /path"`, an absolute path outside the repo): the contract does not require
/// path validation, and this test deliberately does not invent that requirement.
#[test]
fn declare_refuses_blank_entries_in_either_set() {
    let valid_write = "crates/hypothesis/src/hypothesis.rs";
    let valid_read = "crates/hypothesis/src/store.rs";

    let cases: [(&str, ScopeDraft); 4] = [
        (
            "empty string in write_paths",
            draft(Some(&[valid_write, ""]), Some(&[valid_read])),
        ),
        (
            "whitespace-only entry in write_paths",
            draft(Some(&["   "]), Some(&[valid_read])),
        ),
        (
            "empty string in read_paths",
            draft(Some(&[valid_write]), Some(&[""])),
        ),
        (
            "tab-and-space-only entry in read_paths",
            draft(Some(&[valid_write]), Some(&["\t "])),
        ),
    ];

    for (case, d) in cases {
        assert_refused(d.declare(), &format!("rule 4 ({case})"));
    }
}

/// Contract §1, the accepted boundary: `read_paths: Some(vec![])` resolves to
/// `Known`. "Reads nothing" is a determined answer, unlike "writes nothing".
///
/// This is the over-blocking counterweight: without it, every refusal test above
/// is satisfied by a `declare` that never returns `Known`, which would be a gate
/// that blocks everything and therefore measures nothing.
///
/// What this does NOT prove: it does not prove the *only* accepted shapes are
/// the ones the contract lists — it establishes one accepted point, not the full
/// boundary. It also does not check that `declare` is pure (that is
/// `interrogate_purity.rs`'s lexical guard).
#[test]
fn declare_accepts_read_paths_answered_as_empty() {
    let d = draft(Some(&["crates/hypothesis/src/hypothesis.rs"]), Some(&[])).declare();

    match d.require() {
        Required::Determined(decl) => {
            assert_eq!(
                decl.write_paths(),
                ["crates/hypothesis/src/hypothesis.rs".to_string()],
                "the declared write surface must survive declaration"
            );
            assert!(
                decl.read_paths().is_empty(),
                "read_paths was answered as empty, and that answer must be kept \
                 as empty rather than refused or back-filled"
            );
        }
        Required::Blocked(verdict) => panic!(
            "an answered-empty read set is a determined answer and must not be \
             refused; declare() blocked with {verdict:?} — this is over-blocking, \
             which makes the refusal tests vacuous"
        ),
    }
}

/// The extraction path and the payload: `require()` is the only way to get the
/// value out, and the `ScopeDeclaration` it yields must carry the paths that
/// were declared — not empty slices.
///
/// Concrete values are asserted (not just lengths) because accessors that return
/// `&[]` unconditionally would satisfy any weaker check while making the
/// declaration useless to every caller.
///
/// What this does NOT prove:
/// - It does not prove `require()` is the only *possible* extraction path. That
///   is a type-level claim, and it belongs to the negative controls under
///   `tests/ui/scope/` (pass 2). Here `require()` is merely the path used.
/// - It says nothing about normalization semantics, because the contract does
///   not specify any: whether `declare` trims, de-duplicates, sorts, or
///   canonicalizes entries is **unstated**. The inputs below are chosen so that
///   every reasonable reading agrees (no padding, no duplicates, already in the
///   caller's order). If the implementation needs to normalize, this expectation
///   must be revisited through the orchestrator rather than edited to match
///   whatever the implementation happens to do.
#[test]
fn declared_scope_preserves_the_declared_paths() {
    let d = draft(
        Some(&[
            "crates/hypothesis/src/hypothesis.rs",
            "crates/hypothesis/src/store.rs",
        ]),
        Some(&["crates/harness-core/src/interrogate.rs"]),
    )
    .declare();

    let decl: ScopeDeclaration = match d.require() {
        Required::Determined(decl) => decl,
        Required::Blocked(verdict) => panic!(
            "a fully answered draft must declare, but declare() blocked with \
             {verdict:?}"
        ),
    };

    assert_eq!(
        decl.write_paths(),
        [
            "crates/hypothesis/src/hypothesis.rs".to_string(),
            "crates/hypothesis/src/store.rs".to_string(),
        ],
        "write_paths() must return the declared write surface verbatim"
    );
    assert_eq!(
        decl.read_paths(),
        ["crates/harness-core/src/interrogate.rs".to_string()],
        "read_paths() must return the declared read surface verbatim"
    );
}
