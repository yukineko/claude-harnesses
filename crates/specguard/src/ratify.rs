//! Prompt ratification — the prompt templates are *meta-canon* (the audit policy
//! that decides what counts as drift). Treating them as canon means a change
//! must be (1) contract-checked, (2) consented to with a rationale, and (3)
//! pinned. This module records that consent (a lock file holding the template
//! fingerprints + the canon commit + the reason) and verifies it before a gated
//! `run`. Human consent is what confers canon authority and terminates the
//! "who audits the auditor" regress; the lock is the pinned record of it.

use crate::similarity;
use anyhow::Result;
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// Hex fingerprint of a template's bytes. Non-cryptographic (FNV-1a, 64-bit via
/// the shared `harness_core::hash`): we only need to detect that a template
/// changed since ratification, not resist adversarial collisions.
pub fn hash(s: &str) -> String {
    format!("{:016x}", harness_core::hash::fnv1a64(s.as_bytes()))
}

/// The ratification lock lives at the repo root and SHOULD be committed: it is
/// the pinned record of human consent to a prompt version.
pub fn lock_path(repo_root: &Path) -> PathBuf {
    repo_root.join(".specguard-prompt.lock")
}

#[derive(Debug, Deserialize)]
pub struct Lock {
    pub audit_hash: String,
    pub decisions_hash: String,
    /// V1 refute template fingerprint. `default` for backward compatibility with
    /// locks written before the verification gates existed: an old lock simply
    /// has no refute policy pinned, so it only re-blocks once the refute gate is
    /// turned on (see [`drifted`]).
    #[serde(default)]
    pub refute_hash: String,
    /// V2 completeness template fingerprint (same backward-compat note).
    #[serde(default)]
    pub completeness_hash: String,
    #[serde(default)]
    pub canon_commit: String,
    #[serde(default)]
    pub date: String,
    #[serde(default)]
    pub reason: String,
    /// The full **ratified template texts** at ratification time — the corpus the
    /// graded gate ([`triage_drift`]) measures a changed template against for a
    /// deterministic precedented-vs-novel decision. Empty for locks written before
    /// the graded gate existed: with no corpus recorded, a drifted template can
    /// only ever be *novel* (falls back to the binary gate's human-ratify path),
    /// so old locks keep their exact prior behavior until re-ratified.
    #[serde(default)]
    pub corpus: Corpus,
}

/// The four prompt-template slots (`audit` / `decisions` / `refute` /
/// `completeness`) — the single shape shared by every per-template record in this
/// module. It is reused (via the aliases below) as:
///  - [`TemplateHashes`]: the templates' fingerprints (drives the cheap, exact
///    binary drift check),
///  - [`TemplateTexts`]: the templates' full texts (drives the graded gate's
///    similarity triage), and
///  - [`Corpus`]: the ratified texts recorded in the lock (the graded gate's
///    precedent).
///
/// A slot is empty when its policy is not part of the live ratified surface (its
/// verify gate is off) or the lock predates the graded gate; consent is scoped to
/// the surface that is actually live. Collapsing the three formerly-identical
/// structs into one means a new template slot is added in exactly one place. The
/// serde field names (`audit`/`decisions`/`refute`/`completeness`, each
/// `#[serde(default)]`) are the on-disk lock keys and are preserved verbatim, so
/// this refactor is behavior-preserving for the `[corpus]` table.
#[derive(Debug, Default, Deserialize)]
pub struct TemplateSlots {
    #[serde(default)]
    pub audit: String,
    #[serde(default)]
    pub decisions: String,
    #[serde(default)]
    pub refute: String,
    #[serde(default)]
    pub completeness: String,
}

/// The ratified template texts recorded in the lock (the precedent corpus for the
/// graded gate). See [`TemplateSlots`].
pub type Corpus = TemplateSlots;

/// The fingerprints of the four prompt templates (meta-canon) at ratification or
/// check time. See [`TemplateSlots`].
pub type TemplateHashes = TemplateSlots;

/// The full texts of the four prompt templates (meta-canon) at ratification or
/// check time. See [`TemplateSlots`].
pub type TemplateTexts = TemplateSlots;

/// Blank out the slots whose verification gate is inactive, so consent is pinned
/// (and precedent recorded) only for the *live* policy surface — an off gate
/// contributes neither a hash nor a text. Mirrors the activation rule in
/// [`drifted`]. Shared by every ratify/accept path so the masking rule lives in
/// one place rather than being re-spelled at each call site.
pub fn mask_inactive(slots: &mut TemplateSlots, refute_active: bool, completeness_active: bool) {
    if !refute_active {
        slots.refute = String::new();
    }
    if !completeness_active {
        slots.completeness = String::new();
    }
}

/// Read the ratification lock, if present and parseable.
pub fn read_lock(repo_root: &Path) -> Option<Lock> {
    let text = std::fs::read_to_string(lock_path(repo_root)).ok()?;
    toml::from_str(&text).ok()
}

/// Write (or overwrite) the lock — the act of ratification. The verify hashes in
/// `h` are empty when their gate is inactive, so consent is pinned only for the
/// live policy surface; activating a gate later leaves its slot empty and forces
/// a fresh ratification (see [`drifted`]).
pub fn write_lock(
    repo_root: &Path,
    h: &TemplateHashes,
    texts: &TemplateTexts,
    canon_commit: &str,
    date: &str,
    reason: &str,
) -> Result<PathBuf> {
    let path = lock_path(repo_root);
    let body = format!(
        "# specguard prompt ratification lock.\n\
         # The prompt templates are meta-canon (the audit + verification policy).\n\
         # This file pins the version a human ratified, with the reason. The\n\
         # refute/completeness hashes are pinned only when their [verify] gate is\n\
         # on. The [corpus] table stores the ratified template texts so the graded\n\
         # gate can measure a later change's similarity to its precedent.\n\
         # Regenerate via `specguard accept-prompt`. Commit this file.\n\
         audit_hash = {}\n\
         decisions_hash = {}\n\
         refute_hash = {}\n\
         completeness_hash = {}\n\
         canon_commit = {}\n\
         date = {}\n\
         reason = {}\n\
         \n\
         [corpus]\n\
         audit = {}\n\
         decisions = {}\n\
         refute = {}\n\
         completeness = {}\n",
        toml_str(&h.audit),
        toml_str(&h.decisions),
        toml_str(&h.refute),
        toml_str(&h.completeness),
        toml_str(canon_commit),
        toml_str(date),
        toml_str(reason),
        toml_str(&texts.audit),
        toml_str(&texts.decisions),
        toml_str(&texts.refute),
        toml_str(&texts.completeness),
    );
    // Atomic tmp-write + rename (shared `harness_core::store` helper, already used
    // across the tree): the corpus payload can be large, so a plain in-place write
    // could expose a truncated / half-written lock to a concurrent reader or a
    // crash. `save_bytes` is best-effort and panic-free (Result-less): on a
    // write/rename failure it silently returns. A bare `path.exists()` check only
    // detects TOTAL absence, so on the common re-ratify path — where a stale (or
    // wrong-typed) file already occupies `path` — a failed overwrite would report a
    // false success. Read the bytes back and require them to equal what we intended
    // to persist, so a write that never landed surfaces as a real error.
    harness_core::store::save_bytes(&path, body.as_bytes());
    match std::fs::read(&path) {
        Ok(got) if got == body.as_bytes() => Ok(path),
        Ok(_) => anyhow::bail!(
            "ratification lock {} was not updated (write did not land)",
            path.display()
        ),
        Err(e) => {
            Err(anyhow::Error::new(e)
                .context(format!("writing ratification lock {}", path.display())))
        }
    }
}

/// Encode `s` as a TOML basic string (double-quoted, with the escapes TOML
/// requires). Used for every scalar written to the lock so arbitrary template
/// text — which may contain quotes, backslashes, or newlines — round-trips
/// through `toml::from_str` unchanged.
fn toml_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The graded-gate verdict for a set of drifted templates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Triage {
    /// Every drifted template strongly matches its ratified precedent → the gate
    /// may auto-ratify (re-pin) and let the run proceed without a human.
    Precedented,
    /// At least one drifted template is a novel/large deviation → route to a human
    /// `accept-prompt`. Carries the names of the novel templates (a subset of the
    /// drifted set) for the operator message.
    Novel(Vec<&'static str>),
}

/// Grade the `drifted` templates (from [`drifted`]) against the lock's ratified
/// `corpus` using the deterministic [`similarity`] metric at `threshold`.
///
/// A drifted template is *precedented* when its new text's best similarity to the
/// recorded corpus is `>= threshold`, else *novel*. The whole change is
/// [`Triage::Precedented`] only if **every** drifted template is precedented — one
/// novel template pulls the entire ratification back to the human path (an
/// auto-ratify must not silently wave through a novel policy just because a
/// sibling template is precedented).
///
/// Determinism: a pure function of `drifted`, `lock.corpus`, `now`, and
/// `threshold`. Note the corpus per template is the single ratified prior text
/// (not a multi-doc corpus), so `best_similarity` here compares the candidate to
/// exactly that precedent; an empty prior (old lock / inactive gate) yields
/// similarity `0.0` → novel, preserving the binary fallback.
pub fn triage_drift(
    drifted: &[&'static str],
    corpus: &Corpus,
    now: &TemplateTexts,
    threshold: f64,
) -> Triage {
    let mut novel = Vec::new();
    for &name in drifted {
        let (prior, candidate) = match name {
            "audit-prompt" => (&corpus.audit, &now.audit),
            "decisions-prompt" => (&corpus.decisions, &now.decisions),
            "refute-prompt" => (&corpus.refute, &now.refute),
            "completeness-prompt" => (&corpus.completeness, &now.completeness),
            // Unknown drift label: treat conservatively as novel.
            _ => {
                novel.push(name);
                continue;
            }
        };
        let precedent = [prior.clone()];
        if similarity::triage(candidate, &precedent, threshold) == similarity::Verdict::Novel {
            novel.push(name);
        }
    }
    if novel.is_empty() {
        Triage::Precedented
    } else {
        Triage::Novel(novel)
    }
}

/// Which templates changed vs the lock (empty = still ratified). The audit and
/// decisions policies are always checked; the verify policies are checked only
/// when their gate is active, so a project that never enables a gate is never
/// asked to ratify inert policy — and turning a gate on (its slot empty in the
/// lock) registers as drift, demanding a fresh, meaningful consent.
pub fn drifted(
    lock: &Lock,
    h: &TemplateHashes,
    refute_active: bool,
    completeness_active: bool,
) -> Vec<&'static str> {
    let mut v = Vec::new();
    if lock.audit_hash != h.audit {
        v.push("audit-prompt");
    }
    if lock.decisions_hash != h.decisions {
        v.push("decisions-prompt");
    }
    if refute_active && lock.refute_hash != h.refute {
        v.push("refute-prompt");
    }
    if completeness_active && lock.completeness_hash != h.completeness {
        v.push("completeness-prompt");
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_deterministic_and_sensitive() {
        assert_eq!(hash("abc"), hash("abc"));
        assert_ne!(hash("abc"), hash("abd"));
        assert_eq!(hash("abc").len(), 16);
    }

    fn lock_with(audit: &str, decisions: &str, refute: &str, completeness: &str) -> Lock {
        Lock {
            audit_hash: audit.into(),
            decisions_hash: decisions.into(),
            refute_hash: refute.into(),
            completeness_hash: completeness.into(),
            canon_commit: String::new(),
            date: String::new(),
            reason: String::new(),
            corpus: Corpus::default(),
        }
    }

    fn texts(audit: &str, decisions: &str, refute: &str, completeness: &str) -> TemplateTexts {
        TemplateTexts {
            audit: audit.into(),
            decisions: decisions.into(),
            refute: refute.into(),
            completeness: completeness.into(),
        }
    }

    fn corpus(audit: &str, decisions: &str, refute: &str, completeness: &str) -> Corpus {
        Corpus {
            audit: audit.into(),
            decisions: decisions.into(),
            refute: refute.into(),
            completeness: completeness.into(),
        }
    }

    fn hashes(audit: &str, decisions: &str, refute: &str, completeness: &str) -> TemplateHashes {
        TemplateHashes {
            audit: audit.into(),
            decisions: decisions.into(),
            refute: refute.into(),
            completeness: completeness.into(),
        }
    }

    #[test]
    fn drifted_flags_changed_audit_and_decisions() {
        let lock = lock_with("a", "d", "", "");
        // Verify gates inactive: only audit + decisions are checked.
        assert!(drifted(&lock, &hashes("a", "d", "z", "z"), false, false).is_empty());
        assert_eq!(
            drifted(&lock, &hashes("x", "d", "", ""), false, false),
            vec!["audit-prompt"]
        );
        assert_eq!(
            drifted(&lock, &hashes("a", "y", "", ""), false, false),
            vec!["decisions-prompt"]
        );
    }

    #[test]
    fn verify_policy_checked_only_when_gate_active() {
        let lock = lock_with("a", "d", "r", "c");
        // Active + matching -> no drift.
        assert!(drifted(&lock, &hashes("a", "d", "r", "c"), true, true).is_empty());
        // Active + changed refute -> flagged; completeness inactive so ignored.
        assert_eq!(
            drifted(&lock, &hashes("a", "d", "X", "Y"), true, false),
            vec!["refute-prompt"]
        );
        // Both active + both changed.
        assert_eq!(
            drifted(&lock, &hashes("a", "d", "X", "Y"), true, true),
            vec!["refute-prompt", "completeness-prompt"]
        );
    }

    #[test]
    fn enabling_gate_against_unpinned_lock_is_drift() {
        // An old/audit-only lock has no refute policy pinned (empty). Turning the
        // refute gate on registers as drift -> forces a fresh ratification.
        let lock = lock_with("a", "d", "", "");
        assert_eq!(
            drifted(&lock, &hashes("a", "d", "r", ""), true, false),
            vec!["refute-prompt"]
        );
    }

    // --- Graded gate (triage_drift) -----------------------------------------

    const RATIFIED_AUDIT: &str =
        "audit the changed area against its canon pointers, quote the rule verbatim, \
         and flag any implementation that contradicts the ratified specification";

    #[test]
    fn precedented_audit_change_auto_ratifies() {
        // A lightly reworded audit template (one clause tweaked) stays close to the
        // ratified precedent -> the whole change is Precedented (auto-ratify path).
        let corpus = corpus(RATIFIED_AUDIT, "", "", "");
        let now = texts(
            "audit the changed area against its canon pointers, quote the rule verbatim, \
             and flag any implementation that contradicts the ratified spec document",
            "",
            "",
            "",
        );
        assert_eq!(
            triage_drift(&["audit-prompt"], &corpus, &now, 0.6),
            Triage::Precedented
        );
    }

    #[test]
    fn novel_audit_change_routes_to_human() {
        // A wholesale rewrite shares little with the precedent -> Novel -> human.
        let corpus = corpus(RATIFIED_AUDIT, "", "", "");
        let now = texts(
            "sample core end to end flows every run and diff perceptual screenshot \
             hashes against a stored baseline to catch visual regressions",
            "",
            "",
            "",
        );
        assert_eq!(
            triage_drift(&["audit-prompt"], &corpus, &now, 0.6),
            Triage::Novel(vec!["audit-prompt"])
        );
    }

    #[test]
    fn one_novel_among_precedented_pulls_whole_change_to_human() {
        // audit is precedented but decisions is a total rewrite: the whole
        // ratification must go to a human (only the novel name is reported).
        let corpus = corpus(
            RATIFIED_AUDIT,
            "pin every decision record to a canon commit",
            "",
            "",
        );
        let now = texts(
            "audit the changed area against its canon pointers, quote the rule verbatim, \
             and flag any implementation that contradicts the ratified spec",
            "an unrelated policy about visual perceptual hashing of screenshots",
            "",
            "",
        );
        assert_eq!(
            triage_drift(&["audit-prompt", "decisions-prompt"], &corpus, &now, 0.6),
            Triage::Novel(vec!["decisions-prompt"])
        );
    }

    #[test]
    fn threshold_one_is_backward_compatible_binary() {
        // At threshold 1.0 only a punctuation/whitespace-identical change is
        // precedented (== the binary gate's "no meaningful drift"); any real edit
        // is Novel -> human, i.e. the historical behavior.
        let corpus = corpus(RATIFIED_AUDIT, "", "", "");
        // Punctuation-only reflow of the same text: still Precedented at 1.0.
        let same = texts(
            "audit the changed area against its canon pointers; quote the rule verbatim, \
             and flag any implementation that contradicts the ratified specification.",
            "",
            "",
            "",
        );
        assert_eq!(
            triage_drift(&["audit-prompt"], &corpus, &same, 1.0),
            Triage::Precedented
        );
        // A one-word content edit: Novel at 1.0.
        let edited = texts(
            "audit the changed module against its canon pointers, quote the rule verbatim, \
             and flag any implementation that contradicts the ratified specification",
            "",
            "",
            "",
        );
        assert_eq!(
            triage_drift(&["audit-prompt"], &corpus, &edited, 1.0),
            Triage::Novel(vec!["audit-prompt"])
        );
    }

    #[test]
    fn empty_precedent_is_always_novel() {
        // Old lock / inactive gate: no recorded corpus text -> similarity 0 ->
        // Novel, preserving the binary fallback.
        let corpus = corpus("", "", "", "");
        let now = texts("any brand new audit policy text here", "", "", "");
        assert_eq!(
            triage_drift(&["audit-prompt"], &corpus, &now, 0.6),
            Triage::Novel(vec!["audit-prompt"])
        );
    }

    #[test]
    fn polarity_flip_routes_to_human_through_graded_gate() {
        // End-to-end through triage_drift at the shipped default threshold (0.85):
        // a single-token semantic inversion of the ratified audit policy must be
        // Novel (human) even though its lexical Jaccard sits above 0.85, closing
        // the graded auto-ratify bypass. The precedent is realistic-length so the
        // one-token flip stays lexically high.
        let ratified = "for the sampled slice of the changed area the auditor should \
                        flag every implementation that contradicts the ratified \
                        specification for that area and then route the finding over to a \
                        reviewer whenever the audit is uncertain of the rule rather than \
                        silently dropping it from the merged report at the end of the run";
        let corpus = corpus(ratified, "", "", "");
        // contradicts -> matches: inverts the audit verdict.
        let flipped = ratified.replace("contradicts", "matches");
        let now = texts(&flipped, "", "", "");
        assert_eq!(
            triage_drift(&["audit-prompt"], &corpus, &now, 0.85),
            Triage::Novel(vec!["audit-prompt"])
        );
        // A benign punctuation/whitespace reflow (polarity unchanged) still
        // auto-ratifies at the same threshold.
        let reflow = texts(&format!("{ratified}."), "", "", "");
        assert_eq!(
            triage_drift(&["audit-prompt"], &corpus, &reflow, 0.85),
            Triage::Precedented
        );
    }

    #[test]
    fn triage_drift_is_deterministic() {
        let corpus = corpus(RATIFIED_AUDIT, "", "", "");
        let now = texts(
            "audit the changed area against its canon pointers, quote the rule verbatim, \
             and flag any implementation that contradicts the ratified spec doc",
            "",
            "",
            "",
        );
        let a = triage_drift(&["audit-prompt"], &corpus, &now, 0.6);
        let b = triage_drift(&["audit-prompt"], &corpus, &now, 0.6);
        assert_eq!(a, b);
    }

    /// Re-review regression (2026-07-10, FIXED): the atomic-write fix
    /// (finding 7) switched `write_lock` to `harness_core::store::save_bytes`
    /// — a `Result`-less, fail-soft writer that swallows internal I/O errors
    /// — and inferred success from `path.exists()` alone. That check only
    /// detects TOTAL absence, not an overwrite failure: if the lock path is
    /// already occupied (the common re-ratify case) and the write can never
    /// land, `path.exists()` stays `true` and `write_lock` reported `Ok` even
    /// though the new content was never persisted. `write_lock` now reads the
    /// bytes back and requires them to equal what it intended to write, so a
    /// failed overwrite propagates as a real `Err`. Reproduced deterministically
    /// by pre-occupying the lock path with a directory, so the rename inside
    /// `save_bytes` can never succeed. See
    /// docs/review-redesign-implementation-items.md, re-review finding 2.
    #[test]
    fn write_lock_reports_error_when_write_silently_fails() {
        let dir = std::env::temp_dir().join(format!(
            "specguard-ratify-write-fail-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        // Pre-occupy the lock path with a directory: `save_bytes`'s internal
        // `rename(&tmp, path)` can never succeed against an existing
        // directory, so the write silently fails while the (directory) path
        // keeps existing.
        std::fs::create_dir_all(lock_path(&dir)).unwrap();

        let h = hashes("ah", "dh", "", "");
        let t = texts("audit body", "decisions body", "", "");
        let result = write_lock(&dir, &h, &t, "deadbeef", "2026-07-10", "reason");

        assert!(
            result.is_err(),
            "write_lock must propagate a write failure instead of reporting success \
             merely because the (unwritten) target path still exists: got {result:?}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn lock_round_trips_corpus_with_special_chars() {
        // The corpus is written via toml_str and must survive parsing back even
        // with quotes, backslashes and newlines in the template text.
        let dir =
            std::env::temp_dir().join(format!("specguard-ratify-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let h = hashes("ah", "dh", "", "");
        let tricky = "line one\n\"quoted\" and a \\backslash\tand tab";
        let t = texts(tricky, "decisions body", "", "");
        write_lock(
            &dir,
            &h,
            &t,
            "deadbeef",
            "2026-07-09",
            "reason \"with\" quotes",
        )
        .unwrap();
        let lock = read_lock(&dir).expect("lock must parse back");
        assert_eq!(lock.audit_hash, "ah");
        assert_eq!(lock.corpus.audit, tricky);
        assert_eq!(lock.corpus.decisions, "decisions body");
        assert_eq!(lock.reason, "reason \"with\" quotes");
        std::fs::remove_dir_all(&dir).ok();
    }
}
