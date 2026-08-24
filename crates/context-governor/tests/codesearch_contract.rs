//! Independent contract tests for `context_governor::codesearch`.
//!
//! Written by a reviewer who is NOT the module's author, against the module's
//! own stated safety claim (its doc comment):
//!
//! > "An empty result is never served. `Lookup::Serve` is reachable only with
//! > at least one hit. Every other outcome ... is a `Lookup::Decline` carrying
//! > *why*, and every decline means the real search must run."
//!
//! These tests exercise: the `symbol_shaped` boundary, the no-empty-serve
//! invariant on `Lookup::serve`, the labelled (never silent) truncation in
//! `render`, the tri-state `load` at the store boundary, and finally a
//! deliberately RED test that pins down what `lookup` must do once its stub is
//! replaced with a real implementation.

use context_governor::codesearch::{load, render, symbol_shaped, Decline, Hit, Lookup};
use harness_core::code_index::{write_index, Symbol};

// ─────────────────────────────────────────────────────────────────────────
// 1. `symbol_shaped` boundaries
// ─────────────────────────────────────────────────────────────────────────

#[test]
fn symbol_shaped_accepts_foo_bar() {
    assert!(symbol_shaped("foo_bar"));
}

#[test]
fn symbol_shaped_accepts_foo_bar_camel() {
    assert!(symbol_shaped("FooBar"));
}

#[test]
fn symbol_shaped_accepts_leading_underscore() {
    assert!(symbol_shaped("_x"));
}

#[test]
fn symbol_shaped_accepts_letter_then_digit() {
    assert!(symbol_shaped("T3"));
}

#[test]
fn symbol_shaped_rejects_empty_string() {
    assert!(!symbol_shaped(""));
}

#[test]
fn symbol_shaped_rejects_leading_digit() {
    assert!(!symbol_shaped("3foo"));
}

#[test]
fn symbol_shaped_rejects_regex_star() {
    assert!(!symbol_shaped("foo.*"));
}

#[test]
fn symbol_shaped_rejects_internal_space() {
    assert!(!symbol_shaped("foo bar"));
}

#[test]
fn symbol_shaped_rejects_leading_caret() {
    assert!(!symbol_shaped("^foo"));
}

#[test]
fn symbol_shaped_rejects_alternation_pipe() {
    assert!(!symbol_shaped("foo|bar"));
}

#[test]
fn symbol_shaped_rejects_trailing_dollar() {
    assert!(!symbol_shaped("foo$"));
}

#[test]
fn symbol_shaped_rejects_path_with_slash_and_dot() {
    assert!(!symbol_shaped("crates/x.rs"));
}

#[test]
fn symbol_shaped_rejects_quoted_string() {
    assert!(!symbol_shaped("\"msg\""));
}

#[test]
fn symbol_shaped_rejects_hyphen() {
    assert!(!symbol_shaped("foo-bar"));
}

#[test]
fn symbol_shaped_rejects_open_paren() {
    assert!(!symbol_shaped("foo("));
}

#[test]
fn symbol_shaped_rejects_embedded_newline() {
    assert!(!symbol_shaped("foo\nbar"));
}

#[test]
fn symbol_shaped_rejects_non_ascii_identifier() {
    // A perfectly legal Rust/Unicode identifier, but symbol_shaped is
    // documented as ASCII-only (`is_ascii_alphabetic` / `is_ascii_alphanumeric`),
    // so a non-ASCII identifier must be rejected, not accepted.
    assert!(!symbol_shaped("関数"));
}

// ─────────────────────────────────────────────────────────────────────────
// 2. The no-empty-serve invariant
// ─────────────────────────────────────────────────────────────────────────

fn sample_hit() -> Hit {
    Hit {
        name: "alpha_helper".to_string(),
        kind: "fn".to_string(),
        file: "src/lib.rs".to_string(),
        line: 42,
        signature: "fn alpha_helper() {".to_string(),
    }
}

#[test]
fn serve_of_empty_vec_is_decline_no_symbol_match() {
    let outcome = Lookup::serve(vec![]);
    assert_eq!(outcome, Lookup::Decline(Decline::NoSymbolMatch));
}

#[test]
fn serve_of_empty_vec_is_not_serve_of_empty_vec() {
    let outcome = Lookup::serve(vec![]);
    // Spelled out explicitly: this must NOT be Serve(vec![]).
    assert_ne!(outcome, Lookup::Serve(vec![]));
}

#[test]
fn serve_of_one_hit_is_serve() {
    let outcome = Lookup::serve(vec![sample_hit()]);
    assert_eq!(outcome, Lookup::Serve(vec![sample_hit()]));
}

#[test]
fn must_fall_through_true_for_every_decline_variant() {
    let variants = vec![
        Decline::NotEnabled,
        Decline::NotSymbolShaped,
        Decline::IndexAbsent,
        Decline::IndexStale,
        Decline::NoSymbolMatch,
        Decline::Unreadable("boom".to_string()),
    ];
    for d in variants {
        let tag = d.tag();
        assert!(
            Lookup::Decline(d).must_fall_through(),
            "Decline::{tag} must report must_fall_through() == true"
        );
    }
}

#[test]
fn must_fall_through_false_for_serve() {
    let outcome = Lookup::Serve(vec![sample_hit()]);
    assert!(!outcome.must_fall_through());
}

// ─────────────────────────────────────────────────────────────────────────
// 3. `render` truncation is labelled, never silent
// ─────────────────────────────────────────────────────────────────────────

fn make_hits(n: usize) -> Vec<Hit> {
    (0..n)
        .map(|i| Hit {
            name: format!("symbol_number_{i}"),
            kind: "fn".to_string(),
            file: format!("crates/some_crate/src/module_{i}.rs"),
            line: i + 1,
            signature: format!(
                "pub fn symbol_number_{i}(argument_one: usize, argument_two: &str) -> Result<(), Error> {{"
            ),
        })
        .collect()
}

#[test]
fn render_of_many_hits_stays_under_cap() {
    // Each rendered line is well over 60 bytes, so a few hundred hits
    // comfortably exceeds MAX_READOUT_BYTES (10_000).
    let hits = make_hits(500);
    let out = render(&hits);
    assert!(
        out.len() <= context_governor::codesearch::MAX_READOUT_BYTES,
        "render output ({} bytes) exceeded MAX_READOUT_BYTES ({} bytes)",
        out.len(),
        context_governor::codesearch::MAX_READOUT_BYTES
    );
}

#[test]
fn render_of_many_hits_labels_truncation_with_true_total() {
    let hits = make_hits(500);
    let out = render(&hits);
    assert!(
        out.contains("truncated"),
        "render output must say 'truncated' when the cap drops hits; got:\n{out}"
    );
    assert!(
        out.contains("500"),
        "render output must carry the true total hit count (500); got:\n{out}"
    );
}

#[test]
fn render_of_small_hit_list_has_no_truncation_notice() {
    let hits = make_hits(3);
    let out = render(&hits);
    assert!(
        !out.contains("truncated"),
        "a small hit list that fits under the cap must not mention truncation; got:\n{out}"
    );
}

// ─────────────────────────────────────────────────────────────────────────
// 4. `load` tri-state at the store boundary
// ─────────────────────────────────────────────────────────────────────────

#[test]
fn load_with_no_index_file_is_index_absent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let result = load(dir.path());
    assert_eq!(result, Err(Decline::IndexAbsent));
}

#[test]
fn load_with_garbage_only_index_is_unreadable_not_ok_not_absent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let index_dir = dir.path().join(".fugu");
    std::fs::create_dir_all(&index_dir).expect("mkdir .fugu");
    let index_path = index_dir.join("code-index.jsonl");
    std::fs::write(&index_path, "not json\n\n   \nalso not json {{{\n").expect("write garbage");

    let result = load(dir.path());
    match result {
        Err(Decline::Unreadable(detail)) => {
            assert!(
                !detail.is_empty(),
                "Unreadable detail should carry a non-empty explanation"
            );
        }
        Ok(symbols) => panic!(
            "a garbage-only index file must not be reported as Ok(empty); got Ok({symbols:?})"
        ),
        Err(other) => {
            panic!("a garbage-only (but present) index file must be Unreadable, not {other:?}")
        }
    }
}

#[test]
fn load_with_one_valid_symbol_line_is_ok_with_that_symbol() {
    let dir = tempfile::tempdir().expect("tempdir");
    let index_dir = dir.path().join(".fugu");
    std::fs::create_dir_all(&index_dir).expect("mkdir .fugu");
    let index_path = index_dir.join("code-index.jsonl");

    let symbol = Symbol {
        name: "alpha_helper".to_string(),
        kind: "fn".to_string(),
        file: "src/lib.rs".to_string(),
        line: 7,
        signature: "fn alpha_helper() {".to_string(),
    };
    write_index(&index_path, std::slice::from_ref(&symbol));

    let result = load(dir.path()).expect("a valid index must load Ok");
    assert_eq!(result, vec![symbol]);
}

// ─────────────────────────────────────────────────────────────────────────
// 5. F->P oracle: `lookup` must serve a hit that the index holds
// ─────────────────────────────────────────────────────────────────────────

/// F->P oracle. Written against the stub — `lookup` then returned
/// `Decline::NotEnabled` unconditionally — and observed failing for exactly
/// that reason before the implementation landed (2026-08-24). It passes now:
/// a symbol-shaped pattern against an index holding a matching declaration
/// `Serve`s that hit.
///
/// Do not weaken this assertion and do not mark it ignored: the observed RED
/// is what makes this GREEN evidence that the property holds, rather than
/// evidence that the test looks at nothing.
#[test]
fn lookup_serves_a_fresh_index_hit() {
    let dir = tempfile::tempdir().expect("tempdir");
    let index_dir = dir.path().join(".fugu");
    std::fs::create_dir_all(&index_dir).expect("mkdir .fugu");
    let index_path = index_dir.join("code-index.jsonl");

    let symbol = Symbol {
        name: "alpha_helper".to_string(),
        kind: "fn".to_string(),
        file: "src/lib.rs".to_string(),
        line: 7,
        signature: "fn alpha_helper() {".to_string(),
    };
    write_index(&index_path, std::slice::from_ref(&symbol));

    let outcome = context_governor::codesearch::lookup(dir.path(), "alpha_helper", 10);

    match outcome {
        Lookup::Serve(hits) => {
            assert!(
                hits.iter().any(|h| h.name == "alpha_helper"),
                "Serve must contain a hit named alpha_helper; got {hits:?}"
            );
        }
        Lookup::Decline(reason) => panic!(
            "lookup() against a fresh index containing a matching symbol must Serve, \
             not Decline({reason:?}) — this failure is expected until lookup() is implemented"
        ),
    }
}

/// Companion property that must hold both now (against the stub) and after
/// `lookup` is implemented: a pattern that is not symbol-shaped must never be
/// served from the index, because the index cannot vouch for non-identifier
/// text (comments, strings, punctuation, regex metacharacters).
#[test]
fn lookup_never_serves_a_non_symbol_shaped_pattern() {
    let dir = tempfile::tempdir().expect("tempdir");
    let index_dir = dir.path().join(".fugu");
    std::fs::create_dir_all(&index_dir).expect("mkdir .fugu");
    let index_path = index_dir.join("code-index.jsonl");

    let symbol = Symbol {
        name: "alpha_helper".to_string(),
        kind: "fn".to_string(),
        file: "src/lib.rs".to_string(),
        line: 7,
        signature: "fn alpha_helper() {".to_string(),
    };
    write_index(&index_path, std::slice::from_ref(&symbol));

    let outcome = context_governor::codesearch::lookup(dir.path(), "alpha.*", 10);
    assert!(
        outcome.must_fall_through(),
        "a non-symbol-shaped pattern must always fall through to the real search; got {outcome:?}"
    );
}
