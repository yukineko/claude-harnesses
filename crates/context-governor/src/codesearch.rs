//! Symbol-lookup short-circuit for `Grep` — the **cost** axis (recompute), not
//! the size axis: it does not shrink the window, it avoids paying for a
//! full-tree scan when a cached symbol index already knows the answer.
//!
//! # Why this module cannot be a two-valued cache
//!
//! The index behind it ([`harness_core::code_index`]) holds **declarations
//! only** — `fn` / `struct` / `enum` / `trait` / `impl` / `mod` / `const` /
//! `static` / `type` / `macro`. It knows nothing about comments, string
//! literals, error messages, config values, or any other text a `Grep` is
//! legitimately looking for.
//!
//! That makes "the index found nothing" **radically different** from "there is
//! nothing to find", and conflating them is the one failure this module exists
//! to prevent. A fresh index returning zero hits for `"connection refused"` is
//! not evidence that the string is absent from the repo; it is evidence that
//! the question was outside the index's vocabulary. So:
//!
//! > **An empty result is never served.** [`Lookup::Serve`] is reachable only
//! > with at least one hit. Every other outcome — including a fresh index with
//! > zero matches — is a [`Lookup::Decline`] carrying *why*, and every decline
//! > means the real search must run.
//!
//! # The direction of "restrictive" is inverted here
//!
//! Elsewhere in this workspace a component that cannot decide resolves to the
//! restricted side (block / deny). This module is a **cache, not a gate**, and
//! the restricted side of a cache is *do the expensive correct thing*.
//! Declining lets the real `Grep` run; blocking it would protect nothing and
//! merely break the caller. So `Decline` is the safe direction, and the type is
//! shaped so the safe direction is also the easy one: [`Lookup`] has no
//! `Default`, no `From<bool>`, and the stub returns `Decline`.
//!
//! Consequently a crash, a timeout, an unreadable store, or an unrecognised
//! query must all end in `Decline` — never in a synthesized empty answer.

use harness_core::code_index::{self, Symbol};
use harness_core::verdict::Determination;
use std::path::{Path, PathBuf};

/// One symbol the index can vouch for, flattened for rendering.
///
/// Deliberately a copy rather than a borrow of [`Symbol`]: the rendered answer
/// outlives the loaded index in the hook path, and the hit count is bounded by
/// `k` so the copy is negligible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    /// Symbol name as declared.
    pub name: String,
    /// One of the [`harness_core::code_index::Symbol::kind`] values.
    pub kind: String,
    /// Repo-relative path of the declaring file.
    pub file: String,
    /// 1-indexed line of the declaration.
    pub line: usize,
    /// Single-line signature as scanned (never joined across lines).
    pub signature: String,
}

impl From<&Symbol> for Hit {
    fn from(s: &Symbol) -> Self {
        Hit {
            name: s.name.clone(),
            kind: s.kind.clone(),
            file: s.file.clone(),
            line: s.line,
            signature: s.signature.clone(),
        }
    }
}

/// Why the index refused to answer. Each variant is a distinct, reportable
/// fact — **not** a shade of "no results".
///
/// Not `#[non_exhaustive]` on purpose: adding a reason should be a compile
/// error at every match site, so a new way of failing cannot be silently
/// funnelled into an existing arm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decline {
    /// The short-circuit is switched off, so the store was never consulted.
    /// A permanent, legitimate state (kill switch), not a transitional one.
    NotEnabled,
    /// The pattern is not a bare identifier, so it may be matching text the
    /// index does not model (regex metacharacters, whitespace, punctuation).
    NotSymbolShaped,
    /// No index has been built for this root.
    IndexAbsent,
    /// An index exists but its fingerprint disagrees with the current tree.
    /// Serving from it would answer about a repo that no longer exists.
    IndexStale,
    /// The index is present and fresh, and matched nothing. This is **not**
    /// "absent from the repo" — see the module docs.
    NoSymbolMatch,
    /// The store could not be read or parsed. Carries the detail.
    Unreadable(String),
}

impl Decline {
    /// A short, stable, machine-greppable tag. Used in the readout so a
    /// declined lookup is attributable after the fact rather than invisible.
    pub fn tag(&self) -> &'static str {
        match self {
            Decline::NotEnabled => "not-enabled",
            Decline::NotSymbolShaped => "not-symbol-shaped",
            Decline::IndexAbsent => "index-absent",
            Decline::IndexStale => "index-stale",
            Decline::NoSymbolMatch => "no-symbol-match",
            Decline::Unreadable(_) => "unreadable",
        }
    }
}

/// The outcome of consulting the index for one `Grep` pattern.
///
/// No `Default` and no `From<bool>`: there is no such thing as a "default"
/// answer here, and a bool cannot carry the reason a decline needs.
#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use]
pub enum Lookup {
    /// The index is fresh, the query was symbol-shaped, and these hits were
    /// found. **Guaranteed non-empty** — constructed only via [`Lookup::serve`].
    Serve(Vec<Hit>),
    /// The real search must run. Carries why.
    Decline(Decline),
}

impl Lookup {
    /// Build a `Serve`, or a `Decline(NoSymbolMatch)` when `hits` is empty.
    ///
    /// This is the only constructor for `Serve`, which is what makes the
    /// "never serve an empty answer" rule a property of the type rather than a
    /// convention every call site has to remember.
    pub fn serve(hits: Vec<Hit>) -> Self {
        if hits.is_empty() {
            Lookup::Decline(Decline::NoSymbolMatch)
        } else {
            Lookup::Serve(hits)
        }
    }

    /// True when the real search still has to run.
    pub fn must_fall_through(&self) -> bool {
        matches!(self, Lookup::Decline(_))
    }
}

/// Is `pattern` a bare Rust identifier, and therefore answerable by a
/// declaration index?
///
/// Deliberately strict — this is the gate that keeps text searches away from a
/// symbol store, so every uncertain shape must answer `false`:
///
/// * empty, or starting with a digit → `false`
/// * anything outside `[A-Za-z0-9_]` → `false` (this rejects every regex
///   metacharacter, path separator, space and quote as a side effect, without
///   needing to enumerate them)
///
/// It accepts `foo_bar`, `FooBar`, `_x`, `T3`; it rejects `foo.*`, `foo bar`,
/// `^foo`, `foo|bar`, `crates/x.rs`, `"msg"`, and the empty string.
pub fn symbol_shaped(pattern: &str) -> bool {
    let mut chars = pattern.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    pattern
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Default index location for `root`, matching the path `fugu-router
/// code-index` already writes so the two share one store instead of building a
/// second one.
pub fn index_path(root: &Path) -> PathBuf {
    root.join(".fugu").join("code-index.jsonl")
}

/// Sidecar meta location for `root` (staleness fingerprint).
pub fn meta_path(root: &Path) -> PathBuf {
    root.join(".fugu").join("code-index.meta.json")
}

/// Consult the index for declarations named exactly `pattern`, under `root`.
///
/// This is the **raw index query**: "what does the store know about this
/// name". It answers about *declarations only*, and it deliberately does NOT
/// decide whether that is an adequate substitute for a `Grep` — see
/// [`grep_substitute`], which is the function a hook must call.
///
/// Matching is **exact name equality**, not the fuzzy token-overlap ranking of
/// [`code_index::search`]. A top-k ranker will happily return the closest
/// symbol it has when the queried name is absent, which at this boundary would
/// manufacture a confident wrong answer; exact equality cannot.
///
/// `k` caps the returned hits. Overloaded names (a method implemented on many
/// types) can legitimately exceed it, so hitting the cap is reported through
/// [`render`]'s truncation notice rather than silently dropping the tail.
pub fn lookup(root: &Path, pattern: &str, k: usize) -> Lookup {
    if !symbol_shaped(pattern) {
        return Lookup::Decline(Decline::NotSymbolShaped);
    }
    let symbols = match load(root) {
        Ok(s) => s,
        Err(decline) => return Lookup::Decline(decline),
    };
    let hits: Vec<Hit> = symbols
        .iter()
        .filter(|s| s.name == pattern)
        .take(k)
        .map(Hit::from)
        .collect();
    // `serve` turns an empty result into `Decline(NoSymbolMatch)`, so a miss
    // can never leave here looking like an authoritative "nothing exists".
    Lookup::serve(hits)
}

/// Whether the index for `root` is in step with the current tree.
///
/// Tri-state on purpose. The sidecar fingerprint is the only machine-checkable
/// freshness evidence there is, so its **absence is not freshness** — it is the
/// absence of evidence, and it is reported as `Undetermined` rather than
/// guessed either way. (Constraint: freshness is never self-reported.)
///
/// Note the fingerprint is content-free — `(path, size, mtime)` only, per
/// [`code_index::fingerprint`] — so it detects a changed tree cheaply but
/// cannot detect an edit that preserves size and mtime. That is a documented
/// weakness of the shared store, not a claim this function makes.
pub fn freshness(root: &Path, current: &[(String, u64, i64)]) -> Determination<bool> {
    let Some(meta) = code_index::read_meta(&meta_path(root)) else {
        return Determination::undetermined(
            "no index meta sidecar: the index cannot be dated, so it cannot be called fresh",
        );
    };
    Determination::known(meta.fingerprint == code_index::fingerprint(current))
}

/// Whether the index may stand in for a real `Grep` of `pattern`.
///
/// # Why this is not simply [`lookup`]
///
/// A `Grep` for `alpha_helper` returns the declaration **and every reference**.
/// The store behind [`lookup`] holds declarations only, so serving its answer
/// as a `Grep` replacement would hand back a strict subset while looking
/// complete — a reader would reasonably conclude the symbol has no callers.
/// That is precisely the "fast but silently wrong" failure this module's docs
/// open by rejecting.
///
/// Closing that gap needs a persisted **reference** store. The enumerator
/// exists (`blastguard::callgraph::enumerate_callers`) but its edges are
/// recomputed per invocation and never written to disk, so obtaining references
/// today costs the very full-tree scan this short-circuit is meant to avoid.
///
/// Until those edges are persisted, this returns [`Decline::NotEnabled`]
/// **unconditionally**, and it does so for a stated reason rather than by
/// omission. Every `Grep` therefore runs for real: correct today, not yet
/// faster. Wiring a hook to [`lookup`] instead of to this function would be the
/// bug — the separation exists so that mistake has to be made on purpose.
pub fn grep_substitute(_root: &Path, _pattern: &str, _k: usize) -> Lookup {
    Lookup::Decline(Decline::NotEnabled)
}

/// Hard cap on the rendered readout, mirroring the documented 10,000-character
/// ceiling on hook output strings.
pub const MAX_READOUT_BYTES: usize = 10_000;

/// Render served hits for the hook's `permissionDecisionReason` — the one
/// channel the docs state is fed back to the model when a `PreToolUse` hook
/// denies.
///
/// Truncation is **labelled, never silent**: if the cap drops hits, the readout
/// says how many were dropped, so the reader cannot mistake a truncated list
/// for the complete set of matches.
pub fn render(hits: &[Hit]) -> String {
    let header = format!(
        "answered from the symbol index ({} declaration(s)); the tree was not scanned",
        hits.len()
    );
    let mut out = String::from(&header);
    let mut shown = 0usize;

    for hit in hits {
        let line = format!("\n{}:{}  {} {}", hit.file, hit.line, hit.kind, hit.name);
        // +64 leaves room for the truncation notice itself, so the notice can
        // never be the thing that overflows the cap.
        if out.len() + line.len() + 64 > MAX_READOUT_BYTES {
            break;
        }
        out.push_str(&line);
        shown += 1;
    }

    if shown < hits.len() {
        out.push_str(&format!(
            "\n[truncated: {} of {} shown, {} omitted by the {}-byte cap]",
            shown,
            hits.len(),
            hits.len() - shown,
            MAX_READOUT_BYTES
        ));
    }
    out
}

/// Load the index for `root` as a tri-state, so an unreadable store is
/// distinguishable from an empty one.
///
/// Split out from [`lookup`] so the store boundary is testable on its own.
pub fn load(root: &Path) -> Result<Vec<Symbol>, Decline> {
    let path = index_path(root);
    if !path.exists() {
        return Err(Decline::IndexAbsent);
    }
    let symbols = code_index::load_index(&path);
    if symbols.is_empty() {
        // An index file that parses to nothing is not a repo with no symbols;
        // it is a store we cannot trust. Do not report it as a clean miss.
        return Err(Decline::Unreadable(format!(
            "index at {} parsed to zero symbols",
            path.display()
        )));
    }
    Ok(symbols)
}
