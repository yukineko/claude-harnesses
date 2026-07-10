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
/// Each entry is `(surface_token, canonical_bucket)`. Distinct surface tokens
/// that mean the same polarity thing (e.g. "greenlight" and "approve") map to
/// the SAME `canonical_bucket`, so swapping one synonym for another inside the
/// same axis still perturbs the signature multiset — closing the synonym-bypass
/// gap where an out-of-set word slipped past a closed exact-token set. The table
/// stays a curated, deterministic, finite list: NO stemming engine, NO
/// embedding, NO fuzzy match — every entry is an explicit literal pair.
///
/// Covers (a) negation & quantifier-authority words (never/always/no/not/none/
/// unless), (b) allow-vs-deny authorization verbs and their synonyms
/// (allow/permit/enable vs deny/reject/block/forbid/refuse/skip, plus
/// approve/greenlight/accept/authorize/sign-off vs forbid/disable/prohibit),
/// (c) the human-vs-machine routing axis and its synonyms
/// (human/manual/person/reviewer vs auto/automatic/automated/machine), (d)
/// obligation modality (must/require/required/mandatory/optional), and (e) the
/// match-vs-contradict audit verdicts (matches/contradict/contradicts). Tokens
/// are compared in the same normalized token space as the shingles (lowercased,
/// alphanumeric runs), so hyphenated forms like "auto-approve" or "sign-off"
/// normalize to separate word tokens — both of which are in the table — and are
/// caught.
const POLARITY_TOKENS: &[(&str, &str)] = &[
    // Negation & authority quantifiers.
    ("never", "never"),
    ("always", "always"),
    ("no", "no"),
    ("not", "not"),
    ("none", "none"),
    ("unless", "unless"),
    ("without", "without"),
    // Authorization verbs: allow-axis (allow vs deny).
    ("allow", "allow"),
    ("allowed", "allow"),
    ("allows", "allow"),
    ("permit", "allow"),
    ("permits", "allow"),
    ("permitted", "allow"),
    ("enable", "allow"),
    ("enabled", "allow"),
    ("enables", "allow"),
    ("deny", "deny"),
    ("denied", "deny"),
    ("denies", "deny"),
    ("reject", "deny"),
    ("rejected", "deny"),
    ("rejects", "deny"),
    ("refuse", "deny"),
    ("refused", "deny"),
    ("refuses", "deny"),
    ("block", "deny"),
    ("blocked", "deny"),
    ("blocks", "deny"),
    ("skip", "deny"),
    ("skipped", "deny"),
    ("skips", "deny"),
    ("forbid", "forbid"),
    ("forbidden", "forbid"),
    ("forbids", "forbid"),
    ("prohibit", "forbid"),
    ("prohibited", "forbid"),
    ("prohibits", "forbid"),
    ("disable", "forbid"),
    ("disabled", "forbid"),
    ("disables", "forbid"),
    ("approve", "approve"),
    ("approved", "approve"),
    ("approves", "approve"),
    ("approval", "approve"),
    ("greenlight", "approve"),
    ("greenlit", "approve"),
    ("accept", "approve"),
    ("accepted", "approve"),
    ("accepts", "approve"),
    ("authorize", "approve"),
    ("authorized", "approve"),
    ("authorizes", "approve"),
    ("sign", "approve"),
    ("signoff", "approve"),
    // Human-vs-machine routing axis.
    ("human", "human"),
    ("manual", "human"),
    ("manually", "human"),
    ("person", "human"),
    ("reviewer", "human"),
    ("auto", "auto"),
    ("automatic", "auto"),
    ("automatically", "auto"),
    ("automated", "auto"),
    ("machine", "auto"),
    // Obligation modality.
    ("must", "must"),
    ("require", "require"),
    ("required", "require"),
    ("requires", "require"),
    ("mandatory", "require"),
    ("optional", "optional"),
    ("should", "should"),
    // Audit verdict axis (match vs contradict).
    ("matches", "match"),
    ("match", "match"),
    ("contradict", "contradict"),
    ("contradicts", "contradict"),
    ("contradiction", "contradict"),
];

/// Maps each canonical polarity *bucket* to the semantic **axis** it belongs to.
/// An axis groups the mutually-exclusive poles of one decision dimension so that
/// swapping poles *within* an axis (e.g. `allow` ↔ `deny`) is order-sensitive
/// while reordering tokens *across* different axes (e.g. `must` vs `human`) is
/// not. That distinction is what lets [`polarity_signature`] catch a cross-clause
/// pole swap which leaves the flat bucket multiset unchanged, without
/// over-flagging a benign reflow that merely reorders unrelated-axis words.
///
/// Every bucket produced by [`POLARITY_TOKENS`] appears here; the axis label is
/// otherwise the bucket itself (a defensive fallback in [`polarity_signature`]).
const POLARITY_AXES: &[(&str, &str)] = &[
    // Negation & quantifier-authority.
    ("never", "neg"),
    ("always", "neg"),
    ("no", "neg"),
    ("not", "neg"),
    ("none", "neg"),
    ("unless", "neg"),
    ("without", "neg"),
    // Authorization / permission (allow vs deny/forbid, approve).
    ("allow", "authz"),
    ("deny", "authz"),
    ("forbid", "authz"),
    ("approve", "authz"),
    // Human-vs-machine routing.
    ("human", "route"),
    ("auto", "route"),
    // Obligation modality.
    ("must", "modal"),
    ("require", "modal"),
    ("optional", "modal"),
    ("should", "modal"),
    // Audit verdict (match vs contradict).
    ("match", "audit"),
    ("contradict", "audit"),
];

/// The polarity **signature** of `s`: for each semantic axis (see
/// [`POLARITY_AXES`]), the *ordered sequence* of polarity buckets that appear on
/// that axis, in text order. Deterministic (a `BTreeMap` keyed by axis, each value
/// a text-ordered `Vec`), seed-free, and a pure function of the text.
///
/// Why per-axis ordered sequences rather than one flat bucket multiset (the
/// previous design): a flat multiset is blind to a *cross-clause pole swap*. When
/// one clause says "allow X" and another "deny Y", exchanging the two verbs to
/// "deny X" / "allow Y" leaves the multiset `{allow:1, deny:1}` — and hence the
/// lexical Jaccard — completely unchanged, so the single most dangerous edit
/// (inverting which thing is allowed vs denied) auto-ratified, skipping the human.
/// Recording the order of buckets *within each axis* makes that swap visible
/// (`[allow, deny]` → `[deny, allow]`), while keeping *different* axes independent
/// so a benign reflow that reorders unrelated words (e.g. "must … to a human" → "a
/// human … must" — different axes, one bucket each) still compares equal.
/// Within-bucket synonym substitutions collapse to the same bucket (via
/// [`POLARITY_TOKENS`]) and remain invisible, exactly as before.
fn polarity_signature(s: &str) -> BTreeMap<&'static str, Vec<&'static str>> {
    let bucket_of: BTreeMap<&'static str, &'static str> = POLARITY_TOKENS.iter().copied().collect();
    let axis_of: BTreeMap<&'static str, &'static str> = POLARITY_AXES.iter().copied().collect();
    let mut sig: BTreeMap<&'static str, Vec<&'static str>> = BTreeMap::new();
    for tok in tokens(s) {
        if let Some(&bucket) = bucket_of.get(tok.as_str()) {
            let axis = axis_of.get(bucket).copied().unwrap_or(bucket);
            sig.entry(axis).or_default().push(bucket);
        }
    }
    sig
}

/// Function words skipped when locating the *object head* a polarity token
/// governs, so a binding lands on the semantically load-bearing noun (e.g.
/// "whitespace", "rewrite") rather than an article or preposition. Deliberately a
/// small curated, deterministic list — NO stemmer, NO external stopword corpus —
/// mirroring the finite-literal-table discipline of [`POLARITY_TOKENS`].
const OBJECT_STOPWORDS: &[&str] = &[
    "a", "an", "the", "to", "of", "in", "on", "at", "by", "for", "from", "into", "it", "its",
    "that", "this", "these", "those", "and", "or", "but", "any", "all", "every", "some", "as",
    "is", "are", "be", "been", "being", "will", "would", "shall", "can", "may", "over", "with",
    "then", "so", "which", "who", "whom", "whose", "before", "after", "while", "during",
];

/// For each polarity token in `s`, bind its canonical *bucket* to the local
/// content tokens it governs — BOTH the **object head** after it (the nearest
/// following content token) AND the **subject head** before it (the nearest
/// preceding content token), each skipping other polarity tokens and
/// [`OBJECT_STOPWORDS`]. Returns `head -> {bucket -> count}` (deterministic
/// `BTreeMap`s, seed-free, allocation-order-independent).
///
/// This closes two bypasses the per-axis [`polarity_signature`] alone cannot see:
///
///  - **Object-swap** (re-review finding 1): the per-axis signature records only
///    *that* an axis carries, say, an `allow` and a `deny` in some order — never
///    *which object* each pole governs. Swapping the two object phrases between
///    two same-axis clauses ("allow X … deny Y" → "allow Y … deny X"), or trading
///    a single-occurrence token across two different axes, leaves every axis
///    sequence unchanged yet inverts which thing is allowed vs denied. The forward
///    object head makes that reattachment visible.
///  - **Subject-swap** (CA-specguard-01): binding only the *forward* object head
///    is blind to a pure SUBJECT swap — exchanging only the two clause subjects
///    ("routine change *requires* …" / "substantive rewrite is *forbidden*" →
///    "substantive rewrite *requires* …" / "routine change is *forbidden*") leaves
///    every verb and every post-verb object untouched, so both the per-axis
///    sequence and the forward bindings are unchanged while the authorization is
///    inverted. Binding each pole to its *subject head* too makes the swapped
///    subject re-attach to the opposite pole, tripping the guard.
///
/// It stays reflow-tolerant: an ordinary reword that changes the surrounding words
/// entirely simply yields *different* heads — and heads are compared only when the
/// SAME head appears in both texts (see [`polarity_preserved`]) — so a benign edit
/// is not flagged, while a preserved subject/object phrase re-bound to the opposite
/// pole is.
fn object_bindings(s: &str) -> BTreeMap<String, BTreeMap<&'static str, usize>> {
    let bucket_of: BTreeMap<&'static str, &'static str> = POLARITY_TOKENS.iter().copied().collect();
    let stop: BTreeSet<&'static str> = OBJECT_STOPWORDS.iter().copied().collect();
    // A content head is any token that is neither a polarity token nor a function
    // word — the noun a pole actually modifies (as object) or is governed by (as
    // subject).
    let is_head = |t: &str| !bucket_of.contains_key(t) && !stop.contains(t);
    let toks = tokens(s);
    let mut out: BTreeMap<String, BTreeMap<&'static str, usize>> = BTreeMap::new();
    for (i, tok) in toks.iter().enumerate() {
        let Some(&bucket) = bucket_of.get(tok.as_str()) else {
            continue;
        };
        // Object head: the nearest FOLLOWING content token (the noun the pole
        // modifies). Subject head: the nearest PRECEDING content token (the clause
        // subject the pole governs). Binding both closes the pure subject-swap
        // bypass (CA-specguard-01) as well as the forward object-swap bypass.
        let forward = toks[i + 1..].iter().find(|t| is_head(t.as_str()));
        let backward = toks[..i].iter().rev().find(|t| is_head(t.as_str()));
        for head in [forward, backward].into_iter().flatten() {
            *out.entry(head.clone())
                .or_default()
                .entry(bucket)
                .or_default() += 1;
        }
    }
    out
}

/// Whether `a` and `b` carry the SAME polarity meaning. Pure and symmetric. When
/// this is `false` the two texts differ in a semantically load-bearing way even
/// if their lexical similarity is high, so the graded gate must not auto-ratify
/// one as a precedent of the other.
///
/// Two independent checks, either failing forces `false`:
///  1. **per-axis signature** ([`polarity_signature`]) — catches an added,
///     removed or synonym-swapped pole and a same-axis pole reordering.
///  2. **object/subject bindings** ([`object_bindings`]) — catches a cross-clause
///     object swap, a cross-axis single-occurrence swap, or a pure SUBJECT swap
///     that leaves every axis sequence unchanged but reattaches a governed
///     subject/object to the opposite pole (re-review finding 1 / CA-specguard-01).
///     Only a head present in BOTH texts with a differing bucket multiset trips it,
///     so a benign reword is tolerated.
pub fn polarity_preserved(a: &str, b: &str) -> bool {
    if polarity_signature(a) != polarity_signature(b) {
        return false;
    }
    let ba = object_bindings(a);
    let bb = object_bindings(b);
    for (head, buckets_a) in &ba {
        if let Some(buckets_b) = bb.get(head) {
            if buckets_a != buckets_b {
                return false;
            }
        }
    }
    true
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

/// Axes on which a *single template* carrying two or more polarity tokens is, by
/// itself, sufficient grounds to force human review — the **Phase 1 deterministic
/// backstop**. `authz` (allow/deny/forbid/approve) and `route` (human/auto) are
/// the two axes that encode *who decides* and *what is permitted*; a template that
/// mentions either axis twice is expressing a *relationship* between two
/// authorization or routing decisions, which is exactly the shape the heuristic
/// Phase 2 guards ([`polarity_preserved`] / [`object_bindings`]) must reason
/// hardest about (cross-clause pole swaps, object reattachment).
const BACKSTOP_AXES: &[&str] = &["authz", "route"];

/// The per-axis polarity-token occurrence count at or above which the Phase 1
/// backstop fires (2: a single template expressing *two* authz or *two* route
/// decisions).
const BACKSTOP_AXIS_COUNT: usize = 2;

/// **Phase 1 deterministic backstop.** Returns `true` — forcing [`Verdict::Novel`]
/// regardless of similarity or [`polarity_preserved`] — when *either* the
/// candidate *or* its closest precedent carries [`BACKSTOP_AXIS_COUNT`] (2) or
/// more polarity tokens on any single [`BACKSTOP_AXES`] axis.
///
/// This is a pure per-axis COUNT and deliberately depends on NO heuristic: it
/// reuses [`polarity_signature`] (whose per-axis `Vec` length *is* that count) and
/// nothing else. The design goal (re-review finding 1, see
/// docs/review-redesign-implementation-items.md 問題1) is that the SAFETY of the
/// graded gate must not rest on the *perfection* of the Phase 2 local-context
/// heuristics: a manual enumeration of bypasses produced three successive novel
/// variants (findings 1 / 1a / 1b), so a blunt non-heuristic count backstops the
/// whole class. The two-clause verb swap (finding 1) and the object-phrase swap
/// (finding 1a) both put two `authz` tokens in one template, so they land here and
/// route to a human WITHOUT consulting [`object_bindings`] at all.
///
/// **Layering — Phase 1 does NOT subsume Phase 2 (multi-defense).** A cross-axis
/// single-occurrence swap (finding 1b: one `authz` token and one `route`/`modal`
/// token trading places) has an axis count of exactly 1 on every axis, so this
/// backstop does NOT fire for it — its safety is carried entirely by the Phase 2
/// [`object_bindings`] reattachment guard (kept green by
/// `zzz_adversarial_probe_cross_axis_single_occurrence_swap_routes_to_human`).
/// The two phases are complementary layers, not substitutes.
fn backstop_forces_novel(candidate: &str, precedent: &str) -> bool {
    for text in [candidate, precedent] {
        let sig = polarity_signature(text);
        for axis in BACKSTOP_AXES {
            if sig.get(*axis).map_or(0, Vec::len) >= BACKSTOP_AXIS_COUNT {
                return true;
            }
        }
    }
    false
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
            // Phase 1 deterministic backstop (heuristic-free): a template carrying
            // two or more polarity tokens on the authz or route axis always routes
            // to a human, regardless of similarity or the Phase 2
            // `polarity_preserved` heuristics. This is layered ON TOP of — not a
            // replacement for — `object_bindings`: finding 1b (a cross-axis
            // single-occurrence swap) has count 1 per axis and is caught ONLY by
            // Phase 2, so both defenses must remain. See `backstop_forces_novel`.
            if backstop_forces_novel(candidate, precedent) {
                return Verdict::Novel;
            }
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
    use proptest::prelude::*;

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

    /// Regression for the CRITICAL two-clause bypass (review finding 1): the
    /// previous flat-multiset polarity signature was blind to a *cross-clause pole
    /// swap*. Here one clause allows a benign edit and another denies a
    /// substantive one; exchanging the two authorization verbs inverts WHICH edit
    /// is waved through while leaving the polarity bucket multiset — and thus the
    /// lexical Jaccard — unchanged. The per-axis ordered signature must route this
    /// to a human (Novel) at the shipped default threshold even though the
    /// similarity stays above it. Under the old flat multiset this auto-ratified.
    #[test]
    fn zzz_adversarial_probe_two_clause_pole_swap_routes_to_human() {
        const THRESHOLD: f64 = 0.85; // the shipped default.

        // A realistic-length precedent with TWO authorization clauses: it `allow`s
        // a benign whitespace-only edit and separately `deny`s a substantive
        // rewrite. Both clauses share the same "without a second human review"
        // context so only the two verbs distinguish them.
        let ratified = "when the graded ratification gate carefully evaluates a large \
                        incoming batch of drifted meta canon template edits during a fully \
                        gated production release run the deterministic triage logic will \
                        allow the routine whitespace only reformatting change to merge into \
                        the pinned meta canon corpus without requiring a second explicit \
                        human review because that particular edit carries no real semantic \
                        authority over the eventual outcome and the very same triage logic \
                        will separately deny the substantive policy rewrite from merging \
                        into that same pinned meta canon corpus without requiring a second \
                        explicit human review because that particular rewrite clearly does \
                        carry real semantic authority over the final audit outcome";
        // Swap ONLY the two authorization verbs between the two clauses: the benign
        // edit is now denied and the substantive one allowed. The {allow:1, deny:1}
        // bucket multiset is unchanged, so the flat-multiset guard (and the
        // Jaccard) stay put — precisely the bypass the per-axis order closes.
        let flipped = ratified
            .replace("will allow the routine", "will deny the routine")
            .replace("will separately deny", "will separately allow");
        let corpus = vec![ratified.to_string()];

        let sim = best_similarity(&flipped, &corpus);
        // The lexical similarity stays high — the two swapped single words barely
        // perturb the shingle set of this long paragraph; the flat-multiset guard
        // WOULD have auto-ratified it.
        assert!(
            sim >= THRESHOLD,
            "expected a high (>= {THRESHOLD}) lexical similarity that WOULD have \
             auto-ratified under the old flat-multiset guard, got {sim}"
        );
        // The flat bucket multiset is identical, so the OLD signature deemed the two
        // texts polarity-equivalent. The per-axis ordered signature does not.
        assert!(
            !polarity_preserved(ratified, &flipped),
            "the cross-clause allow<->deny swap must perturb the polarity signature"
        );
        // ...so the graded gate routes the inversion to a human.
        assert_eq!(
            triage(&flipped, &corpus, THRESHOLD),
            Verdict::Novel,
            "cross-clause allow<->deny swap must route to a human (Novel) despite \
             similarity {sim}"
        );
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

    /// A within-bucket synonym swap (no axis flip) preserves the polarity
    /// signature: "greenlight" collapses to the same bucket as "approve", so
    /// this is a legitimate benign reword, not a bypass.
    #[test]
    fn synonym_swap_within_same_bucket_preserves_polarity() {
        assert!(polarity_preserved(
            "the reviewer will approve the change",
            "the reviewer will greenlight the change"
        ));
        assert!(polarity_preserved(
            "route it to a human for review",
            "route it to a reviewer for review"
        ));
        assert!(polarity_preserved(
            "the policy must deny the request",
            "the policy must reject the request"
        ));
    }

    /// Regression for the synonym-bypass hole: an inversion that swaps to an
    /// OUT-OF-THE-ORIGINAL-SET synonym on the opposite polarity axis (e.g.
    /// "human" -> "greenlight" is not an axis flip by itself, but a genuine
    /// axis synonym like "deny" -> "permit" or "human" -> "machine" IS) must
    /// still be caught now that the widened table maps synonyms to their axis
    /// bucket, closing the gap where only the exact original word was covered.
    #[test]
    fn synonym_based_axis_flip_detected() {
        // "human" -> "machine": both map into the human/auto axis via synonyms,
        // but land in DIFFERENT buckets ("human" vs "auto") so this is a real flip.
        assert!(!polarity_preserved(
            "route it to a human for explicit consent",
            "route it to a machine for explicit consent"
        ));
        // "deny" -> "permit": permit is a synonym of allow, opposite axis of deny.
        assert!(!polarity_preserved(
            "the policy must deny the request",
            "the policy must permit the request"
        ));
        // "approve" -> "skip" (skip is a deny-axis synonym).
        assert!(!polarity_preserved(
            "the auditor will approve the change",
            "the auditor will skip the change"
        ));
    }

    /// Re-review regression (2026-07-10, FIXED): the per-axis ordered
    /// signature ([`POLARITY_AXES`]) binds a polarity token to its *axis and
    /// position within that axis*, but never to the clause/object it actually
    /// modifies. Swapping only the OBJECT phrases between two same-axis clauses
    /// — leaving both verbs in their original clause positions — inverts which
    /// edit is allowed vs denied while leaving each axis's token *sequence*
    /// unchanged (`[allow, deny]` stays `[allow, deny]`; only the surrounding
    /// non-polarity words move). The [`object_bindings`] reattachment guard now
    /// binds each pole to the object head it governs, so this swap perturbs the
    /// signature and routes to a human. See
    /// docs/review-redesign-implementation-items.md, re-review finding 1(a).
    #[test]
    fn zzz_adversarial_probe_object_phrase_swap_routes_to_human() {
        const THRESHOLD: f64 = 0.85;

        let ratified = "when the graded ratification gate carefully evaluates a large \
                        incoming batch of drifted meta canon template edits during a fully \
                        gated production release run the deterministic triage logic will \
                        allow the whitespace change to merge into the pinned meta canon \
                        corpus without requiring a second explicit human review because \
                        that particular edit carries no real semantic authority over the \
                        eventual outcome and the very same triage logic will separately \
                        deny the substantive rewrite from merging into that same pinned \
                        meta canon corpus without requiring a second explicit human review \
                        because that particular rewrite clearly does carry real semantic \
                        authority over the final audit outcome of the whole release";
        // Swap ONLY the two (short, same-length) object phrases between the
        // clauses; both verbs ("allow", "deny") stay exactly where they
        // were. The now-allowed edit is the substantive rewrite and the
        // now-denied edit is the whitespace change -- the dangerous
        // inversion -- yet the per-axis token sequence for the authz axis is
        // untouched: still `[allow, deny]`.
        let flipped = ratified
            .replace(
                "allow the whitespace change",
                "allow the substantive rewrite",
            )
            .replace("deny the substantive rewrite", "deny the whitespace change");
        let corpus = vec![ratified.to_string()];

        let sim = best_similarity(&flipped, &corpus);
        assert!(
            sim >= THRESHOLD,
            "expected a high (>= {THRESHOLD}) lexical similarity that WOULD have \
             auto-ratified, got {sim}"
        );
        // Desired: the object-phrase swap must perturb the polarity
        // signature (it inverts which edit is allowed vs denied) so the gate
        // routes it to a human. Currently still `true` (the bug).
        assert!(
            !polarity_preserved(ratified, &flipped),
            "the object-phrase swap must perturb the polarity signature even though \
             each axis's token sequence is unchanged"
        );
        assert_eq!(
            triage(&flipped, &corpus, THRESHOLD),
            Verdict::Novel,
            "object-phrase swap must route to a human (Novel) despite similarity {sim}"
        );
    }

    /// Re-review regression (2026-07-10, FIXED): the per-axis signature treats
    /// different axes as fully independent (by design, so a benign reflow
    /// reordering unrelated-axis words compares equal). When each of two
    /// DIFFERENT axes has only a single occurrence, swapping their tokens across
    /// two clauses (e.g. a modal-axis `require` and an authz-axis `forbid` trade
    /// places) changes neither axis's *sequence* (each still has exactly one
    /// entry, in the same per-axis order). The [`object_bindings`] guard closes
    /// this: each pole is bound to the object head it governs, so trading the two
    /// tokens re-binds a preserved object phrase to the opposite pole and
    /// perturbs the signature. See
    /// docs/review-redesign-implementation-items.md, re-review finding 1(b).
    #[test]
    fn zzz_adversarial_probe_cross_axis_single_occurrence_swap_routes_to_human() {
        const THRESHOLD: f64 = 0.85;

        let ratified = "when the graded ratification gate evaluates a drifted meta canon \
                        template during a fully gated production release run the policy \
                        states plainly that a routine formatting change will require a \
                        second explicit paper record from the release captain before it \
                        merges into the pinned corpus and separately states that a \
                        substantive rewrite affecting the audit trail is one the \
                        automation will forbid from merging without any further delay \
                        at all under the current threshold for the release";
        // Swap the modal-axis token "require" (one occurrence in the whole
        // text) with the authz-axis token "forbid" (also one occurrence)
        // across the two clauses. Each axis still contributes exactly one
        // bucket entry -- a single-element sequence is trivially "in the
        // same order" both before and after -- so neither axis's recorded
        // sequence changes, even though the swap inverts which clause
        // demands a paper record vs blocks outright.
        let flipped = ratified
            .replace(
                "will require a second explicit paper record",
                "will forbid a second explicit paper record",
            )
            .replace("will forbid from merging", "will require from merging");
        let corpus = vec![ratified.to_string()];

        let sim = best_similarity(&flipped, &corpus);
        assert!(
            sim >= THRESHOLD,
            "expected a high (>= {THRESHOLD}) lexical similarity that WOULD have \
             auto-ratified, got {sim}"
        );
        // Desired: a cross-axis swap that inverts both clauses' polarity
        // must perturb the signature. Currently still `true` (the bug),
        // because each axis is checked independently and each still holds
        // exactly one (unchanged-position) entry.
        assert!(
            !polarity_preserved(ratified, &flipped),
            "the cross-axis single-occurrence swap must perturb the polarity signature"
        );
        assert_eq!(
            triage(&flipped, &corpus, THRESHOLD),
            Verdict::Novel,
            "cross-axis swap must route to a human (Novel) despite similarity {sim}"
        );
    }

    /// Regression for CA-specguard-01 (the pure SUBJECT-swap bypass):
    /// [`object_bindings`] previously bound each polarity token only to the object
    /// head AFTER the verb (`toks[i+1..]`), never to the clause SUBJECT before it.
    /// Swapping ONLY the two clause subjects between two clauses — leaving every
    /// verb and all post-verb text untouched — inverts the authorization while
    /// leaving the per-axis signature AND the forward object bindings identical.
    /// The Phase-1 backstop does NOT fire (authz=1, route=0), so this auto-ratified
    /// a reversed policy. Binding each pole to its subject head as well makes the
    /// swap visible, routing it to a human. See CA-specguard-01.
    #[test]
    fn zzz_adversarial_probe_subject_phrase_swap_routes_to_human() {
        const THRESHOLD: f64 = 0.85;

        // Two clauses sharing the same tail context; only the SUBJECT distinguishes
        // them. Clause A: the routine formatting change *requires* paperwork
        // (lenient). Clause B: the substantive rewrite is *forbidden* (strict).
        let ratified = "when the graded ratification gate evaluates a drifted meta canon \
                        template during a fully gated production release run the ratification \
                        policy states plainly that the routine formatting change will require \
                        a second explicit paper record from the release captain before \
                        merging into the pinned meta canon corpus and the policy separately \
                        states that the substantive semantic rewrite will forbid the \
                        unreviewed direct merge into that same pinned meta canon corpus \
                        under the current threshold for this whole release";
        // Swap ONLY the two clause subjects ("routine formatting change" <->
        // "substantive semantic rewrite"); both verbs ("require", "forbid") and all
        // post-verb text stay exactly where they were. Now the substantive rewrite
        // merely REQUIRES paperwork (lenient) while the routine change is FORBIDDEN
        // -- a dangerous authorization inversion -- yet each axis's token sequence
        // (modal=[require], authz=[forbid]) and every forward object head is
        // unchanged.
        let flipped = ratified
            .replace(
                "the routine formatting change will require",
                "the substantive semantic rewrite will require",
            )
            .replace(
                "the substantive semantic rewrite will forbid",
                "the routine formatting change will forbid",
            );
        let corpus = vec![ratified.to_string()];

        let sim = best_similarity(&flipped, &corpus);
        assert!(
            sim >= THRESHOLD,
            "expected a high (>= {THRESHOLD}) lexical similarity that WOULD have \
             auto-ratified, got {sim}"
        );
        // The subject swap must perturb the polarity bindings even though each
        // axis's token sequence and every forward object head is unchanged.
        assert!(
            !polarity_preserved(ratified, &flipped),
            "the subject-phrase swap must perturb the polarity signature/bindings"
        );
        assert_eq!(
            triage(&flipped, &corpus, THRESHOLD),
            Verdict::Novel,
            "subject-phrase swap must route to a human (Novel) despite similarity {sim}"
        );
    }

    /// The full end-to-end regression: a synonym-based inversion embedded in a
    /// realistic-length template (so lexical Jaccard stays high, mirroring the
    /// adversarial probe above) routes to Novel at the shipped default
    /// threshold even though NEITHER word is literally the same as the other —
    /// closing the bypass an independent review flagged (out-of-set synonym
    /// slips past a closed exact-token set).
    #[test]
    fn synonym_inversion_routes_to_novel_at_shipped_threshold() {
        const THRESHOLD: f64 = 0.85;

        struct Case {
            ratified: &'static str,
            flipped: &'static str,
            what: &'static str,
        }
        let cases = [
            Case {
                ratified: "when a novel policy edit is detected during a gated run the \
                           auditor consults the ratification lock and finds no matching \
                           precedent so it must route the decision to a human before it \
                           pins the new meta canon version into the ratification lock \
                           file together with the recorded reason for the audit trail",
                flipped: "when a novel policy edit is detected during a gated run the \
                          auditor consults the ratification lock and finds no matching \
                          precedent so it must route the decision to a greenlight before it \
                          pins the new meta canon version into the ratification lock \
                          file together with the recorded reason for the audit trail",
                what: "human -> greenlight (out-of-set synonym of approve)",
            },
            Case {
                ratified: "the change triggered audit reads the canon pointers for the \
                           changed area and then must deny any implementation change that \
                           drifts from the ratified canon and quote the violated rule \
                           verbatim so the operator can read the precise divergence and \
                           decide whether to accept it or send the finding back for repair",
                flipped: "the change triggered audit reads the canon pointers for the \
                          changed area and then must permit any implementation change that \
                          drifts from the ratified canon and quote the violated rule \
                          verbatim so the operator can read the precise divergence and \
                          decide whether to accept it or send the finding back for repair",
                what: "deny -> permit (out-of-set synonym of allow)",
            },
            Case {
                ratified: "for the sampled slice of the changed area the auditor should \
                           always ask a reviewer to approve every implementation that \
                           matches the ratified specification for that area before the \
                           finding is merged into the report at the end of the audit run",
                flipped: "for the sampled slice of the changed area the auditor should \
                          always ask a reviewer to skip every implementation that \
                          matches the ratified specification for that area before the \
                          finding is merged into the report at the end of the audit run",
                what: "approve -> skip (out-of-set synonym of deny)",
            },
        ];

        for c in &cases {
            let corpus = vec![c.ratified.to_string()];
            let sim = best_similarity(c.flipped, &corpus);
            assert!(
                sim >= THRESHOLD,
                "{}: expected a high (>= {THRESHOLD}) lexical similarity that WOULD \
                 have auto-ratified without the polarity guard, got {sim}",
                c.what
            );
            assert_eq!(
                triage(c.flipped, &corpus, THRESHOLD),
                Verdict::Novel,
                "{}: synonym-based polarity flip must route to a human (Novel) despite \
                 similarity {sim}",
                c.what
            );
        }
    }

    // --- Phase 1 deterministic backstop (re-review finding 1, 問題1) -----------

    /// Phase 1 deterministic backstop: a template carrying two or more polarity
    /// tokens on the authz or route axis is forced to Novel by a pure per-axis
    /// COUNT, with NO reliance on the Phase 2 [`object_bindings`] heuristic. This
    /// proves finding 1 (two-clause verb swap) and finding 1a (object-phrase
    /// swap) — both of which put TWO authz tokens in one template — route to a
    /// human even if the local-context heuristic were absent. See
    /// docs/review-redesign-implementation-items.md, re-review finding 1 / 問題1.
    #[test]
    fn phase1_backstop_forces_novel_on_two_axis_tokens_without_object_bindings() {
        // Two authz tokens (allow + deny) in one template — the shape of finding
        // 1 / 1a. The backstop predicate fires on the COUNT ALONE; object_bindings
        // is not consulted.
        let two_authz = "the triage logic will allow the whitespace change and \
                         separately deny the substantive rewrite";
        assert!(
            backstop_forces_novel(two_authz, "an unrelated benign precedent template"),
            "two authz tokens in the candidate must trip the Phase 1 backstop by count alone"
        );
        // Two route tokens (human + auto) in one template; here the *precedent*
        // side trips it (the backstop inspects both texts).
        let two_route = "route the benign edit to auto and the risky edit to a human";
        assert!(
            backstop_forces_novel("an unrelated benign precedent template", two_route),
            "two route tokens in the precedent must trip the Phase 1 backstop by count alone"
        );

        // End-to-end via triage: even at a fully-permissive threshold (0.0) with a
        // lexically IDENTICAL precedent (similarity 1.0, polarity trivially
        // preserved), a two-authz template still routes to a human. WITHOUT the
        // backstop this would auto-ratify (sim >= 0 && polarity_preserved holds).
        let corpus = vec![two_authz.to_string()];
        assert_eq!(
            triage(two_authz, &corpus, 0.0),
            Verdict::Novel,
            "the Phase 1 backstop must override an otherwise-precedented match"
        );
    }

    /// The Phase 1 backstop is deliberately blunt but must NOT over-block a
    /// template that carries at most ONE authz/route token: a single
    /// authorization or routing clause (count 1) stays eligible for auto-ratify,
    /// so benign single-clause edits remain Precedented. (Complements the benign
    /// reword / synonym tests, which also carry <= 1 token per backstop axis, and
    /// documents why those keep auto-ratifying.)
    #[test]
    fn phase1_backstop_leaves_single_axis_clause_precedented() {
        // One authz token (deny) only — backstop does not fire.
        let one_authz = "the policy must deny the unreviewed request before merge";
        assert!(!backstop_forces_novel(one_authz, one_authz));
        let corpus = vec![one_authz.to_string()];
        // A benign whitespace/reflow edit keeps polarity and stays Precedented.
        let benign = "the policy must deny the unreviewed request, before merge";
        assert_eq!(triage(benign, &corpus, 0.85), Verdict::Precedented);

        // One route token (human) only — backstop does not fire either.
        let one_route = "route the novel policy edit to a human for consent";
        assert!(!backstop_forces_novel(one_route, one_route));
    }

    // Object nouns drawn for the fuzzer below; deliberately none is itself a
    // polarity token (see POLARITY_TOKENS) so a random draw cannot perturb the
    // per-axis token counts and change what is being tested.
    const FUZZ_NOUNS: &[&str] = &[
        "whitespace",
        "rewrite",
        "comment",
        "indent",
        "rename",
        "import",
        "typo",
        "spacing",
        "heading",
        "refactor",
    ];

    proptest! {
        /// Property (re-review finding 1, 問題1): no shuffle of clauses and no
        /// cross-axis object exchange can produce a genuinely pole-INVERTED
        /// template that the graded gate auto-ratifies. For every generated
        /// inversion — same-axis (`allow`<->`deny`) OR cross-axis
        /// (`require`<->`forbid`) — embedded in a long scaffold so the lexical
        /// similarity stays in the dangerous `>= threshold` regime, `triage` must
        /// return `Novel`. This machine-fuzzes the manual enumeration that
        /// produced three successive bypasses (findings 1 / 1a / 1b): Phase 1 (the
        /// count backstop) and Phase 2 (`object_bindings`) together must leave NO
        /// inverted case `Precedented`, so a fourth hand-missed variant cannot
        /// silently auto-ratify.
        #[test]
        fn fuzz_pole_inversion_never_auto_ratifies(
            a_idx in 0..FUZZ_NOUNS.len(),
            b_idx in 0..FUZZ_NOUNS.len(),
            // Filler length variation: shuffles/pads the surrounding tokens
            // (clause reordering) without touching the poles or objects.
            pad in 0usize..6,
            // Same-axis (both authz) or cross-axis (authz + modal) poles.
            cross_axis in any::<bool>(),
        ) {
            prop_assume!(a_idx != b_idx);
            const THRESHOLD: f64 = 0.85;
            let obj_a = FUZZ_NOUNS[a_idx];
            let obj_b = FUZZ_NOUNS[b_idx];
            let filler = "meta canon template edit during a fully gated production release run "
                .repeat(pad + 3);
            // v1/v2 are the two clause verbs. Cross-axis pairs a modal (`require`)
            // with an authz (`forbid`) — each a single occurrence, so the Phase 1
            // count backstop does NOT fire and Phase 2 must carry it (finding 1b
            // shape). Same-axis uses two authz verbs (finding 1 / 1a shape), which
            // the backstop catches.
            let (v1, v2) = if cross_axis {
                ("require", "forbid")
            } else {
                ("allow", "deny")
            };
            let ratified = format!(
                "{filler} the triage logic will {v1} the {obj_a} change and {filler} \
                 will separately {v2} the {obj_b} rewrite {filler}"
            );
            // Genuine inversion: swap ONLY the two object nouns between the
            // clauses; both verbs stay put, so the pole that governs each object is
            // inverted while the token multiset is unchanged.
            let flipped = format!(
                "{filler} the triage logic will {v1} the {obj_b} change and {filler} \
                 will separately {v2} the {obj_a} rewrite {filler}"
            );
            let corpus = vec![ratified.clone()];
            let sim = similarity(&flipped, &ratified);
            // The core safety property: a genuine pole inversion must NEVER
            // auto-ratify, at any similarity. Because `Precedented` holds iff
            // (!backstop AND sim >= threshold AND polarity_preserved), asserting
            // `Novel` for every inverted input is *exactly* the DoD property — that
            // the fuzzer generates no case where sim >= threshold and
            // polarity_preserved is true yet the meaning is inverted. (We do not
            // gate on sim >= threshold: identical repeated filler collapses in the
            // shingle SET, so Jaccard is not guaranteed high — but the property is
            // strictly stronger without that gate. Same-axis inversions trip the
            // Phase 1 count backstop; cross-axis single-occurrence ones are caught
            // by the Phase 2 object_bindings guard — both layers exercised here.)
            prop_assert_eq!(
                triage(&flipped, &corpus, THRESHOLD),
                Verdict::Novel,
                "pole inversion (cross_axis={}) must route to a human, sim={}",
                cross_axis,
                sim
            );
        }
    }
}
