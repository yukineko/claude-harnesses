//! Runtime behavior tests for `ScopeDraft::declare` (t3, API contract §1/§1.1/§1.2).
//!
//! The type-level half of done_criteria item 3 — that a `ScopeDeclaration`
//! cannot be *fabricated* — is pinned by `scope_compile_fail.rs` and (in pass 2)
//! the negative controls under `tests/ui/scope/`. That half is not sufficient on
//! its own: a sealed type whose only constructor happily answers `Known` for an
//! empty `write_paths` is still a fail-open. The seal would guarantee that every
//! declaration came from `declare`, and `declare` would guarantee nothing.
//!
//! These tests are the other half: the four `Undetermined` rules the contract
//! fixes, each identified by the machine-readable rule tag from §1.1, the
//! deterministic precedence between them, the boundary positive that stops the
//! trivial over-blocking implementation, and the accessor round-trip that stops
//! the trivial empty-accessor implementation.
//!
//! **Expected to FAIL TO COMPILE until `ScopeDraft` / `ScopeDeclaration` /
//! `declare` exist** in `harness_core::interrogate` per the contract. That is
//! the intended RED (E0432, unresolved import), and it is a genuine Fail→Pass:
//! unlike a trybuild `compile_fail` fixture — which would "pass" on that same
//! unresolved import and therefore prove nothing — an ordinary `#[test]` cannot
//! be satisfied by failing to compile. Nothing here can pass for the wrong
//! reason.
//!
//! Cross-cutting caveat, stated once here and referenced from each test below.
//! Because §1.1 requires a rule tag, the refusal tests now pin *which* rule
//! fired, so an implementation cannot satisfy them without actually classifying
//! its input. What they still cannot show is that anything is *ever* accepted:
//! a `declare` that classifies correctly and then refuses even a fully answered
//! draft passes every refusal test here. `declare_accepts_read_paths_answered_as_empty`
//! and `declared_scope_preserves_the_declared_paths` are the counterweight —
//! they fail for exactly that implementation. Neither group is meaningful
//! without the other.

use harness_core::interrogate::{ScopeDeclaration, ScopeDraft};
use harness_core::verdict::{Determination, Required};

/// The four machine-readable rule tags fixed by contract §1.1. A conforming
/// `Undetermined` reason contains exactly one of these as a substring; human
/// prose may surround it freely.
const ALL_TAGS: [&str; 4] = [
    "scope:write_paths_unasked", // rule 1
    "scope:write_paths_empty",   // rule 2
    "scope:read_paths_unasked",  // rule 3
    "scope:blank_entry",         // rule 4
];

/// Build a draft from optional string slices, so each test states only the shape
/// it cares about. `None` means "the question was never asked"; `Some(&[])`
/// means "asked and answered with nothing".
fn draft(write: Option<&[&str]>, read: Option<&[&str]>) -> ScopeDraft {
    fn own(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }
    ScopeDraft {
        write_paths: write.map(own),
        read_paths: read.map(own),
    }
}

/// Assert that `declare` refused, and that it reported **exactly** the expected
/// rule tag: the expected tag is present and none of the other three are.
///
/// Requiring the absence of the others is what stops a reason that lists every
/// tag unconditionally — such a reason would "contain" whatever a test looked
/// for while identifying nothing, which is the fail-open shape of a check that
/// answers everything.
fn assert_refused_with_tag(d: Determination<ScopeDeclaration>, expected_tag: &str, case: &str) {
    debug_assert!(
        ALL_TAGS.contains(&expected_tag),
        "test bug: {expected_tag:?} is not one of the contract's tags"
    );

    let reason = match d {
        Determination::Undetermined(why) => why.as_str().to_string(),
        Determination::Known(decl) => panic!(
            "{case}: declare() must answer Undetermined, but answered \
             Known({decl:?}). An unresolved scope must not collapse into a \
             permissive declaration — \"could not determine\" is not \"clean\"."
        ),
    };

    assert!(
        reason.contains(expected_tag),
        "{case}: the Undetermined reason must carry the rule tag {expected_tag:?} \
         (contract §1.1) so tests can observe WHICH rule fired; reason was: {reason:?}"
    );

    let also_present: Vec<&str> = ALL_TAGS
        .iter()
        .copied()
        .filter(|tag| *tag != expected_tag)
        .filter(|tag| reason.contains(tag))
        .collect();
    assert!(
        also_present.is_empty(),
        "{case}: exactly one rule tag is allowed (contract §1.1), but alongside \
         {expected_tag:?} the reason also carried {also_present:?}; reason was: \
         {reason:?}"
    );
}

/// Contract §1 rule 1: `write_paths: None` — the question was never asked.
/// §1.1 requires the reason to carry `scope:write_paths_unasked`.
///
/// What this does NOT prove: it does not check the human prose surrounding the
/// tag (only that the tag is present and the other three are not), so it says
/// nothing about whether the message is actually helpful to an operator. Tag
/// matching is substring containment, so a reason embedding the tag inside a
/// longer token would also satisfy it. It does not verify that the give-up was
/// recorded to `harness_core::undetermined` telemetry — which happens only if
/// the implementation mints via `Determination::undetermined`, and is unobserved
/// here. See the module-level caveat for why the refusal tests need the
/// acceptance tests.
#[test]
fn declare_refuses_when_write_paths_was_never_asked() {
    let d = draft(None, Some(&["crates/hypothesis/src/store.rs"])).declare();
    assert_refused_with_tag(d, "scope:write_paths_unasked", "rule 1 (write_paths: None)");
}

/// Contract §1 rule 2: `write_paths: Some(vec![])` — asked, and answered
/// "nothing". This is the rule that matters most: "there is nothing to write"
/// must not be recorded as a satisfied write surface. An empty set read as
/// "nothing wrong" is the exact fail-open shape CLAUDE.md §3 names.
///
/// What this does NOT prove: the same caveats as rule 1 (prose unchecked,
/// substring matching, telemetry unobserved). It also does not, by itself,
/// establish the asymmetry with `read_paths` — that empty *reads* are accepted
/// while empty *writes* are refused is only shown together with
/// `declare_accepts_read_paths_answered_as_empty`.
#[test]
fn declare_refuses_when_write_paths_is_answered_as_empty() {
    let d = draft(Some(&[]), Some(&["crates/hypothesis/src/store.rs"])).declare();
    assert_refused_with_tag(
        d,
        "scope:write_paths_empty",
        "rule 2 (write_paths: Some(vec![]))",
    );
}

/// Contract §1 rule 3: `read_paths: None` — the question was never asked.
/// Distinct from `Some(vec![])`, which the contract accepts.
///
/// What this does NOT prove: the same caveats as rule 1 (prose unchecked,
/// substring matching, telemetry unobserved).
#[test]
fn declare_refuses_when_read_paths_was_never_asked() {
    let d = draft(Some(&["crates/hypothesis/src/hypothesis.rs"]), None).declare();
    assert_refused_with_tag(d, "scope:read_paths_unasked", "rule 3 (read_paths: None)");
}

/// Contract §1 rule 4: any entry in either set is empty or whitespace-only. A
/// blank path is not a declared path; accepting one would let a caller satisfy
/// the write-surface requirement with a placeholder.
///
/// Every case keeps the *other* field a valid answer, so rules 1–3 cannot be
/// what fires — and the tag assert confirms that rather than assuming it.
///
/// What this does NOT prove: prose unchecked, substring matching, telemetry
/// unobserved (as rule 1). It does not enumerate every whitespace form (no
/// `\r`, no non-breaking space, no other unicode whitespace) — it checks the
/// empty string, spaces, and a tab. It says nothing about entries that are
/// non-blank but meaningless (`"."`, a nonexistent path, an absolute path
/// outside the repo): §1.2 forbids `declare` from doing any path validation, so
/// this test deliberately does not invent that requirement. Per §1.2 an entry
/// with *surrounding* whitespace but non-blank content is a valid entry, and
/// that direction is covered by `declare_keeps_padded_but_nonblank_entries`.
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
        assert_refused_with_tag(
            d.declare(),
            "scope:blank_entry",
            &format!("rule 4 ({case})"),
        );
    }
}

/// Contract §1.1 precedence: when several rules apply at once, the reported tag
/// is the **first matching rule in the contract's table order** (1 → 2 → 3 → 4).
/// Without this, "report one of the applicable rules" would vary per
/// implementation and the tag would not be a stable observable.
///
/// The `(write_paths: Some(vec![]), read_paths: None)` case is the one the
/// contract revision calls out explicitly: rule 2 and rule 3 both apply, and
/// rule 2 must win.
///
/// What this does NOT prove: it covers four overlapping combinations, not all
/// of them, and no three-way or four-way overlap. Prose, substring matching, and
/// telemetry caveats are as in rule 1.
#[test]
fn declare_reports_the_first_matching_rule_in_table_order() {
    let cases: [(&str, ScopeDraft, &str); 4] = [
        (
            "rules 1+3 both apply (both questions unasked) -> rule 1 wins",
            draft(None, None),
            "scope:write_paths_unasked",
        ),
        (
            "rules 2+3 both apply (empty writes, unasked reads) -> rule 2 wins",
            draft(Some(&[]), None),
            "scope:write_paths_empty",
        ),
        (
            "rules 3+4 both apply (unasked reads, blank write entry) -> rule 3 wins",
            draft(Some(&["", "crates/hypothesis/src/hypothesis.rs"]), None),
            "scope:read_paths_unasked",
        ),
        (
            "rules 2+4 both apply (empty writes, blank read entry) -> rule 2 wins",
            draft(Some(&[]), Some(&[""])),
            "scope:write_paths_empty",
        ),
    ];

    for (case, d, expected_tag) in cases {
        assert_refused_with_tag(d.declare(), expected_tag, case);
    }
}

/// Contract §1, the accepted boundary: `read_paths: Some(vec![])` resolves to
/// `Known`. "Reads nothing" is a determined answer, unlike "writes nothing".
///
/// This is the over-blocking counterweight: without it, every refusal test above
/// is satisfied by a `declare` that classifies correctly and then never returns
/// `Known`, which would be a gate that blocks everything and therefore measures
/// nothing.
///
/// What this does NOT prove: it does not prove the *only* accepted shapes are
/// the ones the contract lists — it establishes accepted points, not the full
/// boundary. It also does not prove `declare` is pure (that is
/// `interrogate_purity.rs`'s lexical guard, which only sees this file's sibling
/// module's literal source text).
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

/// Contract §1.2: `declare` does not trim. An entry with surrounding whitespace
/// but non-blank content is a valid entry and is kept **verbatim**, including its
/// padding. This is the discriminating case between "whitespace-only entries are
/// refused" (rule 4) and "entries are silently trimmed" (forbidden by §1.2) —
/// the two are easy to conflate in an implementation that trims first and checks
/// emptiness afterwards, which would both accept `"  a  "` and rewrite it.
///
/// What this does NOT prove: it does not cover the other normalizations §1.2
/// forbids (de-duplication and sorting), which
/// `declared_scope_preserves_the_declared_paths` covers instead.
#[test]
fn declare_keeps_padded_but_nonblank_entries() {
    let d = draft(Some(&["  crates/a.rs  "]), Some(&["\tcrates/b.rs"])).declare();

    match d.require() {
        Required::Determined(decl) => {
            assert_eq!(
                decl.write_paths(),
                ["  crates/a.rs  ".to_string()],
                "§1.2 forbids trimming: the padded entry must be preserved verbatim"
            );
            assert_eq!(
                decl.read_paths(),
                ["\tcrates/b.rs".to_string()],
                "§1.2 forbids trimming: the padded entry must be preserved verbatim"
            );
        }
        Required::Blocked(verdict) => panic!(
            "a padded but non-blank entry is a real entry (§1.2) and must not be \
             refused; declare() blocked with {verdict:?}"
        ),
    }
}

/// The extraction path and the payload: `require()` is the only way to get the
/// value out, and the `ScopeDeclaration` it yields must carry the paths that
/// were declared — element for element.
///
/// Concrete values are asserted (not just lengths) because accessors that return
/// `&[]` unconditionally would satisfy any weaker check while making the
/// declaration useless to every caller.
///
/// Per contract §1.2, `declare` performs **no** normalization: no trimming, no
/// de-duplication, no sorting, no path canonicalization. So exact element-wise
/// equality with the input — including the duplicate and the caller's ordering
/// below — is the contract, not an accident of the inputs chosen. An
/// implementation that needs to normalize must be referred back to the
/// orchestrator rather than have this expectation edited to match it.
///
/// What this does NOT prove: it does not prove `require()` is the only
/// *possible* extraction path. That is a type-level claim belonging to the
/// negative controls under `tests/ui/scope/` (pass 2); here `require()` is
/// merely the path used.
#[test]
fn declared_scope_preserves_the_declared_paths() {
    // Deliberately out of sorted order and containing a duplicate, so a `declare`
    // that sorts or de-duplicates fails here (§1.2).
    let d = draft(
        Some(&[
            "crates/hypothesis/src/store.rs",
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
            "crates/hypothesis/src/store.rs".to_string(),
            "crates/hypothesis/src/hypothesis.rs".to_string(),
            "crates/hypothesis/src/store.rs".to_string(),
        ],
        "write_paths() must return the declared write surface verbatim: same \
         elements, same order, duplicates intact (§1.2 forbids normalization)"
    );
    assert_eq!(
        decl.read_paths(),
        ["crates/harness-core/src/interrogate.rs".to_string()],
        "read_paths() must return the declared read surface verbatim"
    );
}
