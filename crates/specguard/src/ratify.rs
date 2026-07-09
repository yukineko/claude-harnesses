//! Prompt ratification — the prompt templates are *meta-canon* (the audit policy
//! that decides what counts as drift). Treating them as canon means a change
//! must be (1) contract-checked, (2) consented to with a rationale, and (3)
//! pinned. This module records that consent (a lock file holding the template
//! fingerprints + the canon commit + the reason) and verifies it before a gated
//! `run`. Human consent is what confers canon authority and terminates the
//! "who audits the auditor" regress; the lock is the pinned record of it.

use crate::similarity;
use anyhow::{Context, Result};
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

/// The ratified template texts recorded in the lock (the precedent corpus for the
/// graded gate). Each field mirrors a slot in [`TemplateHashes`]; a slot is empty
/// when its policy was not part of the ratified surface (its gate was off) or the
/// lock predates the graded gate.
#[derive(Debug, Default, Deserialize)]
pub struct Corpus {
    #[serde(default)]
    pub audit: String,
    #[serde(default)]
    pub decisions: String,
    #[serde(default)]
    pub refute: String,
    #[serde(default)]
    pub completeness: String,
}

/// The fingerprints of the four prompt templates (meta-canon) at ratification or
/// check time. The verify hashes are empty when their gate is inactive — consent
/// is scoped to the policy surface that is actually live.
pub struct TemplateHashes {
    pub audit: String,
    pub decisions: String,
    pub refute: String,
    pub completeness: String,
}

/// The full texts of the four prompt templates (meta-canon) at ratification or
/// check time. Parallel to [`TemplateHashes`]: hashes drive the (cheap, exact)
/// binary drift check; texts drive the graded gate's similarity triage. A text is
/// empty when its gate is inactive, mirroring the hash convention.
pub struct TemplateTexts {
    pub audit: String,
    pub decisions: String,
    pub refute: String,
    pub completeness: String,
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
    std::fs::write(&path, body).with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
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
