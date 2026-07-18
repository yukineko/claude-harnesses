//! Consensus merge-conflict resolution: the schema + resolve mutation for the
//! shared human/policy review surface that a real merge conflict (design
//! 625aa170 B) OR a gated mid-flight overlap (decision A) routes into.
//!
//! ## Why this exists
//!
//! Today condukt's trial-merge (`worktree::merge`) simply `bail!`s on a
//! conflict: the merge stops, but the conflict is a silent local-only degrade —
//! nobody but the running session ever sees it, and there is no path to resolve
//! it out-of-band. Decision A additionally makes a detected mid-flight
//! actual-diff overlap HOLD the second task's merge and route it into the SAME
//! review surface. This module gives both a durable, resolvable entry.
//!
//! A [`MergeConflictEntry`] (append-only, `merge_conflicts.jsonl`) records ONE
//! blocked merge — its conflicted files and both sides' diffs (byte-bounded) —
//! so a human (`overwatch resolve-merge-conflict`) or condukt's policy can pick
//! a resolution. The resolution is a SEPARATE append-only stream
//! (`merge_conflict_resolutions.jsonl`) joined by `conflict_id`, mirroring
//! `disposition.rs`/`append_disposition` (findings ↔ dispositions): the OPEN
//! set is entries with no resolution, and `resolve` is an idempotent
//! check-then-append under `LeaseLock`. The `bail!` (block) is preserved on the
//! condukt side — recording an entry makes the conflict visible/resolvable, it
//! does not un-block the merge.
//!
//! This module owns the schema + pure helpers only; persistence lives in
//! `store.rs`. overwatch cannot depend on condukt (cycle), so these shared
//! types live here and condukt writes them.

use serde::{Deserialize, Serialize};

/// Default byte cap for an inline diff stored in a [`MergeConflictEntry`].
/// Diffs are truncated to keep the JSONL bounded (design Q6: byte-capped inline
/// diffs, not a blob path).
pub const DIFF_BYTE_CAP: usize = 8 * 1024;

/// Truncate `diff` to at most `cap` bytes on a char boundary, appending a
/// `…[truncated N bytes]` marker when it was shortened. Pure and total: never
/// splits a UTF-8 code point, never panics.
pub fn truncate_diff(diff: &str, cap: usize) -> String {
    if diff.len() <= cap {
        return diff.to_string();
    }
    // Walk back to a char boundary at or below `cap`.
    let mut end = cap;
    while end > 0 && !diff.is_char_boundary(end) {
        end -= 1;
    }
    let removed = diff.len() - end;
    format!("{}\n…[truncated {removed} bytes]", &diff[..end])
}

/// Where a blocked merge came from: a genuine git 3-way conflict, or a gated
/// mid-flight actual-diff overlap (decision A). Both surface under the SAME
/// `[merge-conflict]` review kind; this marker lets the resolver tell them
/// apart (an overlap that git would merge cleanly can be fast-approved).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ConflictOrigin {
    /// A real `git merge` 3-way conflict (pre-flight trial merge failed).
    MergeConflict,
    /// A gated mid-flight overlap between two in-flight worktrees (decision A):
    /// no git conflict yet, but the second merge is HELD for review.
    RuntimeOverlap,
}

impl ConflictOrigin {
    /// Short token used in summaries.
    pub fn token(self) -> &'static str {
        match self {
            ConflictOrigin::MergeConflict => "merge-conflict",
            ConflictOrigin::RuntimeOverlap => "runtime-overlap",
        }
    }
}

/// Which side a resolution picks.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResolveChoice {
    /// Keep the default branch's version (`merge -s ours` / skip).
    Ours,
    /// Take the feature branch's version (`merge -X theirs` / checkout).
    Theirs,
    /// A human/worker resolves conflict markers by hand, then commits.
    Manual,
}

impl ResolveChoice {
    /// Parse the CLI-facing `--choose` value. Unknown values are rejected.
    pub fn parse_cli(raw: &str) -> anyhow::Result<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "ours" => Ok(Self::Ours),
            "theirs" => Ok(Self::Theirs),
            "manual" => Ok(Self::Manual),
            other => Err(anyhow::anyhow!(
                "unknown resolve choice {other:?}: expected ours | theirs | manual"
            )),
        }
    }

    /// Canonical snake_case label used in JSON output.
    pub fn label(self) -> &'static str {
        match self {
            Self::Ours => "ours",
            Self::Theirs => "theirs",
            Self::Manual => "manual",
        }
    }
}

/// Who decided a resolution.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DecidedBy {
    /// A human via `overwatch resolve-merge-conflict`.
    Human,
    /// condukt's policy (Escalate/Block only — never an Auto pick-side).
    Policy,
}

impl DecidedBy {
    /// Parse the CLI-facing `--by` value (default Human). Unknown values rejected.
    pub fn parse_cli(raw: &str) -> anyhow::Result<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "human" => Ok(Self::Human),
            "policy" => Ok(Self::Policy),
            other => Err(anyhow::anyhow!(
                "unknown decided-by {other:?}: expected human | policy"
            )),
        }
    }
}

/// One recorded blocked merge (append-only). The OPEN set is the entries with
/// no matching [`MergeConflictResolution`] (joined by `conflict_id`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MergeConflictEntry {
    /// Stable identifier (caller-assigned, e.g. `<run>/<branch>/<ts>`).
    pub conflict_id: String,
    /// Where this blocked merge came from.
    pub origin: ConflictOrigin,
    /// The run whose merge was blocked.
    pub run_id: String,
    /// The feature branch that could not merge (the "theirs" side).
    pub branch: String,
    /// The default branch it was merging into (the "ours" side).
    pub default_branch: String,
    /// The frozen merge-base SHA both diffs are taken against.
    pub base_ref: String,
    /// The conflicted (or overlapping) files.
    pub conflicted_files: Vec<String>,
    /// `base...default_branch` diff (byte-bounded) — the "ours" side.
    pub diff_ours: String,
    /// `base...branch` diff (byte-bounded) — the "theirs" side.
    pub diff_theirs: String,
    /// Unix timestamp when recorded.
    pub ts: i64,
}

/// A resolution of a [`MergeConflictEntry`], joined by `conflict_id`. Its own
/// append-only stream (mirrors dispositions resolving findings).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MergeConflictResolution {
    /// The conflict_id this resolves (join key).
    pub conflict_id: String,
    /// The chosen side.
    pub choice: ResolveChoice,
    /// Who decided.
    pub decided_by: DecidedBy,
    /// Optional free-text note.
    pub note: Option<String>,
    /// Unix timestamp of resolution.
    pub ts: i64,
}

/// Compute the OPEN set: entries whose `conflict_id` has NO resolution.
/// Preserves input order. Pure and deterministic.
pub fn open_entries(
    entries: &[MergeConflictEntry],
    resolutions: &[MergeConflictResolution],
) -> Vec<MergeConflictEntry> {
    use std::collections::BTreeSet;
    let resolved: BTreeSet<&str> = resolutions.iter().map(|r| r.conflict_id.as_str()).collect();
    entries
        .iter()
        .filter(|e| !resolved.contains(e.conflict_id.as_str()))
        .cloned()
        .collect()
}

/// CLI glue for `overwatch resolve-merge-conflict`: record a human/policy
/// resolution of a blocked merge (join key: `conflict_id`). Fail-soft on the
/// store write (mirrors `disposition_cli::record`): a write error is warned to
/// stderr but returns `Ok(())`. An UNKNOWN `--choose`/`--by` is a genuine input
/// error and is rejected with `Err`. `now` is the caller-supplied timestamp
/// (same `store::now()` source) so this stays testable without a wall clock.
pub fn record_resolution(
    conflict_id: String,
    choose: &str,
    by: &str,
    note: Option<String>,
    now: i64,
) -> anyhow::Result<()> {
    let choice = ResolveChoice::parse_cli(choose)?;
    let decided_by = DecidedBy::parse_cli(by)?;
    let cwd = std::env::current_dir()?;
    let resolution = MergeConflictResolution {
        conflict_id: conflict_id.clone(),
        choice,
        decided_by,
        note,
        ts: now,
    };
    match crate::store::append_merge_conflict_resolution(&cwd, &resolution) {
        Ok(()) => {
            println!(
                "{}",
                serde_json::json!({
                    "resolved": true,
                    "conflict_id": conflict_id,
                    "choice": choice.label(),
                })
            );
        }
        Err(e) => {
            eprintln!(
                "overwatch: WARNING could not record merge-conflict resolution (continuing): {e}"
            );
            println!(
                "{}",
                serde_json::json!({ "resolved": false, "reason": "store-write-failed" })
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, origin: ConflictOrigin, ts: i64) -> MergeConflictEntry {
        MergeConflictEntry {
            conflict_id: id.to_string(),
            origin,
            run_id: "runA".to_string(),
            branch: "condukt/t2".to_string(),
            default_branch: "main".to_string(),
            base_ref: "base".to_string(),
            conflicted_files: vec!["shared.rs".to_string()],
            diff_ours: "ours".to_string(),
            diff_theirs: "theirs".to_string(),
            ts,
        }
    }

    #[test]
    fn parse_choice_accepts_known_rejects_unknown() {
        assert_eq!(
            ResolveChoice::parse_cli("ours").unwrap(),
            ResolveChoice::Ours
        );
        assert_eq!(
            ResolveChoice::parse_cli(" Theirs ").unwrap(),
            ResolveChoice::Theirs
        );
        assert_eq!(
            ResolveChoice::parse_cli("MANUAL").unwrap(),
            ResolveChoice::Manual
        );
        assert!(ResolveChoice::parse_cli("auto").is_err());
    }

    #[test]
    fn truncate_diff_bounds_and_marks() {
        let short = "abc";
        assert_eq!(truncate_diff(short, 8), "abc");
        let long = "x".repeat(100);
        let out = truncate_diff(&long, 10);
        assert!(out.starts_with(&"x".repeat(10)));
        assert!(out.contains("truncated"), "must mark truncation: {out}");
        assert!(out.len() < long.len() + 40);
    }

    #[test]
    fn truncate_diff_respects_char_boundary() {
        // Multi-byte chars: cap in the middle of one must not panic / split.
        let s = "áéíóú".repeat(10); // each char 2 bytes
        let out = truncate_diff(&s, 5);
        assert!(out.contains("truncated"));
    }

    #[test]
    fn open_entries_excludes_resolved() {
        let entries = vec![
            entry("c1", ConflictOrigin::MergeConflict, 100),
            entry("c2", ConflictOrigin::RuntimeOverlap, 200),
        ];
        let resolutions = vec![MergeConflictResolution {
            conflict_id: "c1".to_string(),
            choice: ResolveChoice::Ours,
            decided_by: DecidedBy::Human,
            note: None,
            ts: 300,
        }];
        let open = open_entries(&entries, &resolutions);
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].conflict_id, "c2");
    }

    #[test]
    fn entry_round_trips_json() {
        let e = entry("c1", ConflictOrigin::RuntimeOverlap, 42);
        let json = serde_json::to_string(&e).unwrap();
        assert!(json.contains("\"origin\":\"runtime-overlap\""));
        let back: MergeConflictEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(e, back);
    }
}
