//! Deterministic textual similarity for the *graded* ratification gate.
//!
//! The binary ratification gate (`ratify.rs`) treats any change to a meta-canon
//! template as drift that demands a fresh human `accept-prompt`. The graded gate
//! softens this: when a changed template is *precedented* — i.e. lexically very
//! close to the version a human already ratified — it can be auto-ratified,
//! reserving human attention for *novel* / large deviations. That triage decision
//! MUST be reproducible and free of any model/network (specguard is a
//! deterministic drift gate), so similarity here is a pure function: a
//! **token-shingle Jaccard** over normalized text.
//!
//! Why shingles rather than a bag of single tokens (as `harness_core::lessons`
//! uses for lexical *search*): a spec/template edit that reorders or lightly
//! rewords prose leaves the single-token set almost unchanged, which would call
//! almost every edit "precedented". Overlapping n-grams (shingles) are sensitive
//! to local structure/word-order, so an insertion or a reworded clause visibly
//! lowers the score — the behavior a *gate* wants. The metric is symmetric,
//! seed-free, allocation-order-independent (a `BTreeSet`), and identical across
//! runs and platforms.

use std::collections::BTreeSet;

/// Shingle width (number of consecutive normalized tokens per shingle). 3 is a
/// standard near-duplicate-detection choice: wide enough that unrelated texts
/// rarely share a shingle, narrow enough that a small localized edit only
/// perturbs a few shingles rather than the whole set.
const SHINGLE_N: usize = 3;

/// Normalize `s` into its ordered token stream: lowercase, split on any
/// non-alphanumeric boundary, drop empties. Deliberately dependency-free and
/// language-agnostic; punctuation and whitespace differences are erased so a
/// reflow or comment-formatting change does not read as a deviation.
fn tokens(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_string())
        .collect()
}

/// The set of `SHINGLE_N`-token shingles of `s`. When the text has fewer than
/// `SHINGLE_N` tokens it cannot form a full shingle, so the whole token stream is
/// used as a single shingle (a short template still compares meaningfully against
/// another short one). Empty text yields an empty set.
fn shingles(s: &str) -> BTreeSet<String> {
    let toks = tokens(s);
    let mut out = BTreeSet::new();
    if toks.is_empty() {
        return out;
    }
    if toks.len() < SHINGLE_N {
        out.insert(toks.join(" "));
        return out;
    }
    for w in toks.windows(SHINGLE_N) {
        out.insert(w.join(" "));
    }
    out
}

/// Jaccard similarity of two shingle sets: |A ∩ B| / |A ∪ B|, in `[0.0, 1.0]`.
/// Two empty texts are defined as identical (`1.0`) — an unchanged empty
/// template is trivially precedented; a non-empty text vs an empty one is `0.0`.
fn jaccard(a: &BTreeSet<String>, b: &BTreeSet<String>) -> f64 {
    let union = a.union(b).count();
    if union == 0 {
        // Both empty: identical by definition.
        return 1.0;
    }
    a.intersection(b).count() as f64 / union as f64
}

/// Deterministic textual similarity of `a` and `b` in `[0.0, 1.0]`.
///
/// Pure, symmetric, seed-free and reproducible: `similarity(a, b)` depends only
/// on the bytes of `a` and `b`. `1.0` iff the two are token-identical (identical
/// texts, or texts differing only in punctuation/whitespace/case); `0.0` iff they
/// share no `SHINGLE_N`-gram.
pub fn similarity(a: &str, b: &str) -> f64 {
    jaccard(&shingles(a), &shingles(b))
}

/// The best (maximum) similarity of `candidate` against a corpus of already-
/// ratified texts — i.e. how strongly `candidate` matches its closest precedent.
/// An empty corpus yields `0.0`: with nothing ratified to match against, every
/// candidate is novel (the gate must fall back to human ratify).
pub fn best_similarity(candidate: &str, corpus: &[String]) -> f64 {
    corpus
        .iter()
        .map(|prior| similarity(candidate, prior))
        .fold(0.0_f64, f64::max)
}

/// The graded triage verdict for one drifted template.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Strongly matches an already-ratified precedent (best similarity `>=`
    /// threshold): safe to auto-ratify.
    Precedented,
    /// Novel / large deviation (best similarity `<` threshold): route to a human.
    Novel,
}

/// Triage a changed template `candidate` against the ratified `corpus` at
/// `threshold`.
///
/// Determinism/edge contract:
/// - `threshold <= 0.0` makes *everything* precedented (graded gate fully
///   permissive); `threshold > 1.0` makes everything novel — Jaccard maxes at
///   `1.0`, so nothing can reach an above-1.0 bar. Together these are the two
///   degenerate ends of the knob.
/// - A `threshold` of `1.0` is the *backward-compatible* setting: only a
///   token-identical (or punctuation/whitespace-only) change is precedented,
///   which is exactly the binary gate's "no meaningful drift" case. Anything else
///   is `Novel` → human, i.e. old behavior.
/// - Empty corpus → always `Novel` (nothing to be precedented by).
pub fn triage(candidate: &str, corpus: &[String], threshold: f64) -> Verdict {
    if best_similarity(candidate, corpus) >= threshold {
        Verdict::Precedented
    } else {
        Verdict::Novel
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_text_is_one() {
        let s = "the audit policy pins meta canon and requires consent";
        assert_eq!(similarity(s, s), 1.0);
    }

    #[test]
    fn punctuation_and_case_only_change_is_one() {
        let a = "Ratify the prompt, then pin it.";
        let b = "ratify   the prompt then pin it";
        assert_eq!(similarity(a, b), 1.0);
    }

    #[test]
    fn disjoint_text_is_zero() {
        let a = "alpha beta gamma delta epsilon";
        let b = "one two three four five six";
        assert_eq!(similarity(a, b), 0.0);
    }

    #[test]
    fn is_symmetric() {
        let a = "audit the spec against the ratified corpus every run";
        let b = "audit the ratified corpus against the spec often";
        assert_eq!(similarity(a, b), similarity(b, a));
    }

    #[test]
    fn is_deterministic_across_calls() {
        let a = "graded triage of the specification layer";
        let b = "graded triage of a specification layer entry";
        assert_eq!(similarity(a, b), similarity(a, b));
    }

    #[test]
    fn small_edit_stays_high_large_edit_drops() {
        let base = "the specguard gate compares a changed spec to the ratified corpus \
                    and auto ratifies precedented specs while routing novel ones to a human";
        // One-word reword: still strongly similar.
        let small = "the specguard gate compares a changed spec to the ratified corpus \
                     and auto ratifies precedented entries while routing novel ones to a human";
        // Wholesale rewrite: little shingle overlap.
        let large = "an unrelated feature adds visual diff sampling for core end to end \
                     flows using perceptual hashing on captured screenshots each run";
        let s_small = similarity(base, small);
        let s_large = similarity(base, large);
        assert!(
            s_small > 0.7,
            "small edit similarity {s_small} should stay high"
        );
        assert!(
            s_large < 0.2,
            "large rewrite similarity {s_large} should drop"
        );
        assert!(s_small > s_large);
    }

    #[test]
    fn best_similarity_picks_closest_precedent() {
        let cand = "audit the ratified corpus against the changed spec each run";
        let corpus = vec![
            "totally unrelated perceptual hash screenshot sampling flow".to_string(),
            "audit the ratified corpus against the changed spec every run".to_string(),
        ];
        let best = best_similarity(cand, &corpus);
        // Closest precedent (differs by a single word) dominates the unrelated one,
        // which shares no shingle (contributes 0.0).
        assert_eq!(best, similarity(cand, &corpus[1]));
        assert!(
            best >= 0.5,
            "best {best} should reflect the near-identical precedent"
        );
        // Empty corpus -> nothing to match -> 0.
        assert_eq!(best_similarity(cand, &[]), 0.0);
    }

    #[test]
    fn triage_precedented_vs_novel() {
        let corpus = vec![
            "the audit policy pins meta canon and requires human consent to change".to_string(),
        ];
        let precedented = "the audit policy pins the meta canon and requires human consent \
                           to change it";
        let novel = "sample core end to end flows and diff perceptual screenshot hashes";
        assert_eq!(triage(precedented, &corpus, 0.5), Verdict::Precedented);
        assert_eq!(triage(novel, &corpus, 0.5), Verdict::Novel);
    }

    #[test]
    fn triage_threshold_one_is_binary_backward_compat() {
        // At threshold 1.0 only a token-identical change is precedented — exactly
        // the binary gate's "no meaningful drift". Any real edit is Novel -> human.
        let ratified = "pin the ratified prompt version with a reason";
        let corpus = vec![ratified.to_string()];
        // Punctuation/case-only change: still token-identical -> Precedented.
        assert_eq!(
            triage(
                "Pin the ratified prompt version, with a reason.",
                &corpus,
                1.0
            ),
            Verdict::Precedented
        );
        // A one-word content edit: Novel at threshold 1.0.
        assert_eq!(
            triage(
                "pin the ratified prompt version with two reasons",
                &corpus,
                1.0
            ),
            Verdict::Novel
        );
    }

    #[test]
    fn triage_threshold_zero_is_fully_permissive() {
        // threshold <= 0 -> everything precedented (even an empty corpus, since
        // best_similarity 0.0 >= 0.0).
        assert_eq!(
            triage("anything at all here", &[], 0.0),
            Verdict::Precedented
        );
    }

    #[test]
    fn triage_empty_corpus_is_novel() {
        // With a positive threshold and nothing ratified, every candidate is novel.
        assert_eq!(triage("some new spec text", &[], 0.5), Verdict::Novel);
    }
}
