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

use std::collections::{BTreeMap, BTreeSet};

/// Polarity / negation / authority tokens whose presence or absence flips the
/// *meaning* of a meta-canon rule even when almost every other token is shared.
///
/// Rationale (the security hole this closes): the graded gate scores similarity
/// with a token-shingle Jaccard, which is a *lexical* measure. On a realistic-
/// length template a single-token semantic-polarity flip — "route it to a human"
/// → "route it to auto approve", or "never auto approve" → "always auto approve"
/// — barely perturbs the shingle set, so the Jaccard stays above a sane
/// threshold (measured 0.85–0.92) and the most dangerous edit auto-ratifies,
/// skipping the human. These are precisely the words that invert an authorization
/// / obligation / routing decision, so a change to their multiset is treated as
/// semantically load-bearing regardless of how high the lexical similarity is.
///
/// The set is deliberately conservative and closed (no stemming/synonyms): it
/// covers (a) negation & quantifier-authority words (never/always/no/not/none/
/// unless), (b) allow-vs-deny authorization verbs (allow/allowed/deny/denied/
/// permit/forbid/reject/approve/approved), (c) the human-vs-machine routing axis
/// (human/auto/manual), (d) obligation modality (must/require/required/mandatory/
/// optional), and (e) the match-vs-contradict audit verdicts (matches/contradict/
/// contradicts). Tokens are compared in the same normalized token space as the
/// shingles (lowercased, alphanumeric runs), so hyphenated forms like
/// "auto-approve" normalize to the tokens "auto" and "approve" — both of which
/// are in the set — and are caught.
const POLARITY_TOKENS: &[&str] = &[
    // Negation & authority quantifiers.
    "never",
    "always",
    "no",
    "not",
    "none",
    "unless",
    "without",
    // Authorization verbs (allow vs deny axis).
    "allow",
    "allowed",
    "allows",
    "deny",
    "denied",
    "denies",
    "permit",
    "permitted",
    "forbid",
    "forbidden",
    "prohibit",
    "prohibited",
    "reject",
    "rejected",
    "approve",
    "approved",
    "approves",
    "approval",
    // Human-vs-machine routing axis.
    "human",
    "auto",
    "manual",
    "manually",
    "automatically",
    // Obligation modality.
    "must",
    "require",
    "required",
    "requires",
    "mandatory",
    "optional",
    "should",
    // Audit verdict axis (match vs contradict).
    "matches",
    "match",
    "contradict",
    "contradicts",
    "contradiction",
];

/// The multiset (token → count) of polarity tokens in `s`, in the same normalized
/// token space as [`shingles`]. Deterministic (a `BTreeMap`), seed-free, and a
/// pure function of the text. Two texts with an identical polarity multiset are
/// "polarity-equivalent"; any add / remove / flip of a polarity token changes the
/// multiset and is therefore detected.
fn polarity_signature(s: &str) -> BTreeMap<&'static str, usize> {
    let present: BTreeSet<&'static str> = POLARITY_TOKENS.iter().copied().collect();
    let mut sig = BTreeMap::new();
    for tok in tokens(s) {
        if let Some(&canon) = present.get(tok.as_str()) {
            *sig.entry(canon).or_insert(0) += 1;
        }
    }
    sig
}

/// Whether `a` and `b` carry the SAME polarity signature (multiset of
/// polarity/negation/authority tokens). Pure and symmetric. When this is `false`
/// the two texts differ in a semantically load-bearing way even if their lexical
/// similarity is high, so the graded gate must not auto-ratify one as a precedent
/// of the other.
pub fn polarity_preserved(a: &str, b: &str) -> bool {
    polarity_signature(a) == polarity_signature(b)
}

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
/// candidate is novel (the gate must fall back to human ratify). Retained as a
/// public helper (and used by the adversarial regression test to demonstrate the
/// high lexical similarity a polarity flip retains); `triage` itself goes through
/// [`best_precedent`] so it can also inspect *which* precedent matched.
#[cfg(test)]
pub fn best_similarity(candidate: &str, corpus: &[String]) -> f64 {
    best_precedent(candidate, corpus)
        .map(|(_, s)| s)
        .unwrap_or(0.0)
}

/// The closest precedent to `candidate` in `corpus` and its similarity, or `None`
/// for an empty corpus. Ties break on the earliest corpus entry (a stable,
/// deterministic scan) so the polarity check in [`triage`] always inspects the
/// same precedent across runs. Used both for the similarity score and to identify
/// *which* ratified text the candidate is being auto-ratified against, so its
/// polarity signature can be compared.
fn best_precedent<'a>(candidate: &str, corpus: &'a [String]) -> Option<(&'a str, f64)> {
    let mut best: Option<(&'a str, f64)> = None;
    for prior in corpus {
        let s = similarity(candidate, prior);
        match best {
            Some((_, bs)) if bs >= s => {}
            _ => best = Some((prior.as_str(), s)),
        }
    }
    best
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
/// A candidate is `Precedented` (safe to auto-ratify) only when BOTH hold:
///  1. its best similarity to a ratified precedent is `>= threshold`, AND
///  2. its **polarity signature is unchanged** relative to that closest
///     precedent (see [`polarity_preserved`] / [`POLARITY_TOKENS`]).
///
/// The second clause is a security guard: lexical Jaccard is nearly blind to a
/// single-token semantic inversion ("route to a human" → "route to auto approve",
/// "never auto approve" → "always auto approve"), so a high similarity alone
/// would auto-ratify the most dangerous edits and skip the human. Any add /
/// remove / flip of a polarity/negation/authority token forces `Novel`
/// **regardless of similarity**, routing the inversion to a human.
///
/// Determinism/edge contract:
/// - `threshold <= 0.0` makes everything that *also preserves polarity*
///   precedented; a polarity flip is still `Novel` even at threshold `0.0` (the
///   guard is not subordinate to the knob). `threshold > 1.0` makes everything
///   novel — Jaccard maxes at `1.0`.
/// - A `threshold` of `1.0` is the *backward-compatible* setting: only a
///   token-identical (or punctuation/whitespace-only) change is precedented — and
///   such a change trivially preserves polarity, so the guard never alters the
///   threshold-1.0 (binary) behavior.
/// - Empty corpus → always `Novel` (nothing to be precedented by; no precedent to
///   compare polarity against).
pub fn triage(candidate: &str, corpus: &[String], threshold: f64) -> Verdict {
    match best_precedent(candidate, corpus) {
        // A precedent exists: precedented only if it clears the bar AND the
        // polarity signature is unchanged relative to that precedent.
        Some((precedent, sim)) => {
            if sim >= threshold && polarity_preserved(candidate, precedent) {
                Verdict::Precedented
            } else {
                Verdict::Novel
            }
        }
        // Empty corpus: no precedent to compare polarity against, so preserve the
        // historical degenerate contract exactly — best similarity is 0.0, hence
        // Precedented only when `threshold <= 0.0` (fully-permissive knob), else
        // Novel. The polarity guard is vacuous with nothing to invert against.
        None => {
            if 0.0 >= threshold {
                Verdict::Precedented
            } else {
                Verdict::Novel
            }
        }
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

    // --- Polarity guard ------------------------------------------------------

    #[test]
    fn polarity_signature_ignores_non_polarity_tokens() {
        // Reflow/reword that keeps the polarity multiset {must, human} is
        // polarity-preserved even though the non-polarity words are reordered and
        // swapped.
        let a = "the auditor must route a novel policy to a human reviewer";
        let b = "a human reviewer must receive any novel policy the auditor routes";
        assert!(polarity_preserved(a, b));
    }

    #[test]
    fn polarity_signature_detects_negation_flip() {
        assert!(!polarity_preserved(
            "never auto approve a novel policy",
            "always auto approve a novel policy"
        ));
        assert!(!polarity_preserved(
            "route it to a human",
            "route it to auto approve"
        ));
        assert!(!polarity_preserved(
            "the change must not contradict the ratified spec",
            "the change must contradict the ratified spec"
        ));
    }

    #[test]
    fn polarity_signature_detects_authority_verb_flip() {
        assert!(!polarity_preserved(
            "deny any implementation that drifts from canon",
            "allow any implementation that drifts from canon"
        ));
        assert!(!polarity_preserved(
            "flag work that contradicts the specification",
            "flag work that matches the specification"
        ));
    }

    #[test]
    fn hyphenated_auto_approve_normalizes_and_is_caught() {
        // "auto-approve" -> tokens auto, approve; a flip to "human" changes both.
        assert!(!polarity_preserved(
            "route to a human reviewer before merging",
            "route to auto-approve before merging"
        ));
    }

    /// Ex-`zzz_adversarial_probe`: previously an `eprintln!`-only probe that
    /// *documented* the graded-gate hole without gating on it. Now an asserting
    /// regression test proving the empirically-measured single-token semantic
    /// polarity flips route to a human (Novel) at the SHIPPED DEFAULT threshold
    /// (0.85) even though their lexical Jaccard sits ABOVE 0.85. Without the
    /// polarity guard each of these auto-ratified (skipping the human).
    #[test]
    fn zzz_adversarial_probe_polarity_flips_route_to_human() {
        const THRESHOLD: f64 = 0.85; // the shipped default (specguard.example.toml).

        // A realistic-length meta-canon precedent; each flip changes ONE token so
        // the Jaccard stays high — exactly the dangerous regime.
        struct Case {
            ratified: &'static str,
            flipped: &'static str,
            what: &'static str,
        }
        // Each `flipped` swaps EXACTLY ONE token of `ratified` (token count
        // unchanged) so the shingle-Jaccard barely moves and stays above 0.85 —
        // the precise regime the guard must catch.
        // Each `flipped` swaps EXACTLY ONE token of `ratified` (token count
        // unchanged) inside a realistic-length paragraph, so the shingle-Jaccard
        // barely moves and stays above 0.85 — the precise regime the guard must
        // catch. A single-token swap perturbs only the few shingle windows that
        // span it, and the longer the surrounding prose the smaller that fraction.
        let cases = [
            Case {
                ratified: "when a novel policy edit is detected during a gated run the \
                           auditor consults the ratification lock and finds no matching \
                           precedent so it must route the decision to a human before it \
                           pins the new meta canon version into the ratification lock \
                           file together with the recorded reason for the audit trail",
                flipped: "when a novel policy edit is detected during a gated run the \
                          auditor consults the ratification lock and finds no matching \
                          precedent so it must route the decision to a auto before it \
                          pins the new meta canon version into the ratification lock \
                          file together with the recorded reason for the audit trail",
                what: "human -> auto",
            },
            Case {
                ratified: "the ratification policy for the graded gate states plainly that \
                           the automation should never auto approve a genuinely novel policy \
                           edit under any threshold and instead defers the whole decision over \
                           to a second reviewer who reads the change and records the reason \
                           into the pinned lock file for the audit trail",
                flipped: "the ratification policy for the graded gate states plainly that \
                          the automation should always auto approve a genuinely novel policy \
                          edit under any threshold and instead defers the whole decision over \
                          to a second reviewer who reads the change and records the reason \
                          into the pinned lock file for the audit trail",
                what: "never -> always",
            },
            Case {
                ratified: "the change triggered audit reads the canon pointers for the \
                           changed area and then must deny any implementation change that \
                           drifts from the ratified canon and quote the violated rule \
                           verbatim so the operator can read the precise divergence and \
                           decide whether to accept it or send the finding back for repair",
                flipped: "the change triggered audit reads the canon pointers for the \
                          changed area and then must allow any implementation change that \
                          drifts from the ratified canon and quote the violated rule \
                          verbatim so the operator can read the precise divergence and \
                          decide whether to accept it or send the finding back for repair",
                what: "deny -> allow",
            },
            Case {
                ratified: "for the sampled slice of the changed area the auditor should \
                           flag every implementation that contradicts the ratified \
                           specification for that area and then route the finding over to a \
                           reviewer whenever the audit is uncertain of the rule rather than \
                           silently dropping it from the merged report at the end of the run",
                flipped: "for the sampled slice of the changed area the auditor should \
                          flag every implementation that matches the ratified \
                          specification for that area and then route the finding over to a \
                          reviewer whenever the audit is uncertain of the rule rather than \
                          silently dropping it from the merged report at the end of the run",
                what: "contradicts -> matches",
            },
        ];

        for c in &cases {
            let corpus = vec![c.ratified.to_string()];
            let sim = best_similarity(c.flipped, &corpus);
            // The hole exists precisely because the lexical similarity is high...
            assert!(
                sim >= THRESHOLD,
                "{}: expected a high (>= {THRESHOLD}) lexical similarity that WOULD \
                 have auto-ratified without the polarity guard, got {sim}",
                c.what
            );
            // ...yet the polarity guard must still route the inversion to a human.
            assert_eq!(
                triage(c.flipped, &corpus, THRESHOLD),
                Verdict::Novel,
                "{}: polarity flip must route to a human (Novel) despite similarity {sim}",
                c.what
            );
        }
    }

    /// Positive companion to the probe: a genuine benign reflow / reword that
    /// keeps the polarity signature unchanged still auto-ratifies at the shipped
    /// default threshold — the guard does not over-block ordinary edits.
    #[test]
    fn benign_reword_with_unchanged_polarity_still_auto_ratifies() {
        const THRESHOLD: f64 = 0.85;
        let ratified = "when a novel policy edit is detected the auditor must \
                        route it to a human for explicit consent before pinning \
                        the new meta canon version to the ratification lock";
        // Punctuation/whitespace reflow only: token-identical, polarity unchanged.
        let benign = "When a novel policy edit is detected, the auditor must \
                      route it to a human for explicit consent, before pinning \
                      the new meta canon version to the ratification lock.";
        let corpus = vec![ratified.to_string()];
        assert!(polarity_preserved(benign, ratified));
        assert_eq!(triage(benign, &corpus, THRESHOLD), Verdict::Precedented);
    }
}
