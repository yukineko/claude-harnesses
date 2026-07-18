//! Mid-flight runtime-conflict detection: the cross-run registry of in-flight
//! ACTUAL changesets, plus the pure overlap computation and the
//! runtime-conflict event schema.
//!
//! ## Why this exists (design 625aa170 — decision A)
//!
//! condukt's schedule-time parallel-safety only clears tasks whose *declared*
//! `touched_files` are disjoint (`schedule::files_conflict`). A worker that
//! edits an *undeclared* file is structurally invisible to that gate: two
//! in-flight worktrees can both touch the same file with neither declaring it,
//! and the second merge silently clobbers the first (last-writer-wins).
//!
//! This module closes that gap on the DETECTION side. Each task, at the moment
//! it transitions to `done` (post-worker, pre-merge — the same edge the
//! diff-risk hook fires on), records the REAL changed-file set of its worktree
//! (`git diff <base>...HEAD --name-only`) into a project-global registry
//! (`active_changesets.json`, keyed by `task_key`). Recording cross-checks the
//! new changeset against every OTHER in-flight (`!merged`, lease-live) entry and
//! emits one [`RuntimeConflictEvent`] per genuine path overlap
//! (`runtime_conflicts.jsonl`). The in-flight set is the overwatch GLOBAL
//! registry (not condukt run state) so it catches cross-RUN / multi-session
//! collisions, lease-filtered by TTL so a crashed run ages out.
//!
//! The persistence + concurrency glue lives in `store.rs`
//! ([`crate::store::record_changeset_and_detect`], under `LeaseLock`); this
//! module owns only the schema and the PURE, unit-testable computation
//! ([`overlap`] / [`detect_conflicts`]) — mirroring `disposition.rs`
//! (data + pure functions, no I/O). overwatch cannot depend on condukt (that
//! would be a dependency cycle), so the new shared types live HERE and condukt
//! writes them.

use crate::store::LEASE_TTL_SECS;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Normalize a repo-relative path for overlap comparison, matching condukt's
/// `schedule::normalize_entry` casefold/`./`-strip/`//`-collapse parity so a
/// path recorded by condukt and a path already in the registry fold to the SAME
/// key when they name the same file. Pure and total.
///
/// Rules (identical to `schedule::normalize_entry`): strip leading `./`
/// segments, collapse consecutive `/` runs to one, then ASCII/Unicode-lowercase
/// (case-only spelling differences fold to one file on case-insensitive
/// filesystems). These are literal paths, not globs, so overlap is a plain set
/// intersection with no glob expansion.
pub fn normalize_path(p: &str) -> String {
    let mut s = p.trim();
    while let Some(rest) = s.strip_prefix("./") {
        s = rest;
    }
    let mut out = String::with_capacity(s.len());
    let mut last_was_slash = false;
    for c in s.chars() {
        if c == '/' {
            if last_was_slash {
                continue;
            }
            last_was_slash = true;
        } else {
            last_was_slash = false;
        }
        out.push(c);
    }
    out.to_lowercase()
}

/// One in-flight task's ACTUAL changed-file set, frozen at the moment the task
/// went `done`. Stored in the project-global `active_changesets.json` registry
/// keyed by [`ActualChangeset::task_key`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActualChangeset {
    /// Stable per-task key (`<run_id>/<task_id>`) — the registry key.
    pub task_key: String,
    /// The run this task belongs to.
    pub run_id: String,
    /// The session that recorded it (used for cross-session attribution).
    pub session_id: String,
    /// The task's worktree branch.
    pub branch: String,
    /// The frozen merge-base SHA the actual diff was taken against (three-dot
    /// `<base>...HEAD`), so a later-moving default branch does not shift the
    /// comparison out from under an already-recorded changeset.
    pub base_ref: String,
    /// The worktree HEAD SHA at record time.
    pub head_sha: String,
    /// The ACTUAL changed files (normalized, repo-relative), from
    /// `git diff <base_ref>...HEAD --name-only`.
    pub files: Vec<String>,
    /// Unix timestamp when this changeset was recorded (also the lease-liveness
    /// clock: an entry older than [`LEASE_TTL_SECS`] is treated as a crashed
    /// run and excluded from overlap checks).
    pub ts: i64,
    /// Whether the task's branch has landed (merged). A merged changeset is no
    /// longer in-flight and is excluded from overlap checks (and pruned).
    #[serde(default)]
    pub merged: bool,
}

impl ActualChangeset {
    /// Construct a changeset, normalizing `files` for overlap parity.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        task_key: String,
        run_id: String,
        session_id: String,
        branch: String,
        base_ref: String,
        head_sha: String,
        files: &[String],
        ts: i64,
    ) -> Self {
        let mut normed: Vec<String> = files.iter().map(|f| normalize_path(f)).collect();
        normed.sort();
        normed.dedup();
        Self {
            task_key,
            run_id,
            session_id,
            branch,
            base_ref,
            head_sha,
            files: normed,
            ts,
            merged: false,
        }
    }
}

/// The registry of all in-flight actual changesets: `task_key -> changeset`.
pub type ChangesetRegistry = BTreeMap<String, ActualChangeset>;

/// One detected mid-flight overlap between two in-flight tasks. Appended to
/// `runtime_conflicts.jsonl` (append-only, one per overlapping pair). Under
/// decision A this is not merely observational: the recording caller (condukt's
/// detection hook) uses a non-empty return to set a merge-hold on task A.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeConflictEvent {
    /// The run of the task being recorded (task A).
    pub run_id: String,
    /// The task being recorded now (the later finisher) — held on overlap.
    pub task_key_a: String,
    /// The other in-flight task it overlaps with.
    pub task_key_b: String,
    /// The normalized files both tasks actually changed (sorted, non-empty).
    pub overlapping_files: Vec<String>,
    /// The frozen base SHA of task A (for the resolver's context).
    pub base_ref: String,
    /// The session that recorded task A.
    pub session_id: String,
    /// Unix timestamp of detection.
    pub ts: i64,
    /// Human-readable detail line.
    pub detail: String,
}

/// Pure set-intersection of two normalized path lists. Both sides are
/// re-normalized (defensive: callers may pass raw paths), intersected, and
/// returned SORTED and de-duplicated so the result is deterministic. These are
/// literal paths — no glob expansion.
pub fn overlap(a: &[String], b: &[String]) -> Vec<String> {
    let set_a: BTreeSet<String> = a.iter().map(|p| normalize_path(p)).collect();
    let set_b: BTreeSet<String> = b.iter().map(|p| normalize_path(p)).collect();
    set_a.intersection(&set_b).cloned().collect()
}

/// Whether a registry entry is still IN-FLIGHT at time `now`: not merged, and
/// its record timestamp within [`LEASE_TTL_SECS`] (a crashed run that left
/// `merged=false` ages out instead of causing phantom overlaps forever). Pure.
fn is_live(entry: &ActualChangeset, now: i64, ttl: i64) -> bool {
    !entry.merged && (now - entry.ts) <= ttl
}

/// Cross-check a freshly-recorded changeset `new` against every OTHER in-flight
/// entry in `registry`, returning one [`RuntimeConflictEvent`] per genuine path
/// overlap. Pure and deterministic (BTreeMap iteration is key-sorted).
///
/// An entry is compared only when it is (a) not `new` itself, and (b) live per
/// [`is_live`] (`!merged` AND within `ttl` of `new.ts`). `new.ts` is the clock
/// (no wall-clock read here — keeps the core testable). Self-overlap and
/// stale/merged peers never produce events.
pub fn detect_conflicts(
    new: &ActualChangeset,
    registry: &ChangesetRegistry,
    ttl: i64,
) -> Vec<RuntimeConflictEvent> {
    let mut events = Vec::new();
    for (key, entry) in registry {
        if key == &new.task_key {
            continue;
        }
        if !is_live(entry, new.ts, ttl) {
            continue;
        }
        let shared = overlap(&new.files, &entry.files);
        if shared.is_empty() {
            continue;
        }
        let detail = format!(
            "mid-flight actual-diff overlap: task '{}' and task '{}' both changed {} undeclared-shared file(s): {}",
            new.task_key,
            entry.task_key,
            shared.len(),
            shared.join(", ")
        );
        events.push(RuntimeConflictEvent {
            run_id: new.run_id.clone(),
            task_key_a: new.task_key.clone(),
            task_key_b: entry.task_key.clone(),
            overlapping_files: shared,
            base_ref: new.base_ref.clone(),
            session_id: new.session_id.clone(),
            ts: new.ts,
            detail,
        });
    }
    events
}

/// Convenience over [`detect_conflicts`] using the default [`LEASE_TTL_SECS`].
pub fn detect_conflicts_default(
    new: &ActualChangeset,
    registry: &ChangesetRegistry,
) -> Vec<RuntimeConflictEvent> {
    detect_conflicts(new, registry, LEASE_TTL_SECS)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cs(task_key: &str, files: &[&str], ts: i64) -> ActualChangeset {
        ActualChangeset::new(
            task_key.to_string(),
            "runA".to_string(),
            "sess".to_string(),
            format!("condukt/{task_key}"),
            "basesha".to_string(),
            "headsha".to_string(),
            &files.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            ts,
        )
    }

    #[test]
    fn normalize_path_folds_dot_slash_dupslash_and_case() {
        assert_eq!(normalize_path("./crates/Foo.rs"), "crates/foo.rs");
        assert_eq!(normalize_path("crates//foo.rs"), "crates/foo.rs");
        assert_eq!(normalize_path("  ./Crates/Bar.RS  "), "crates/bar.rs");
        assert_eq!(normalize_path("a.rs"), "a.rs");
    }

    #[test]
    fn overlap_names_the_shared_file() {
        let a = vec!["crates/x/src/main.rs".to_string(), "a.rs".to_string()];
        let b = vec!["A.rs".to_string(), "crates/y.rs".to_string()];
        // "a.rs" ~ "A.rs" fold together via casefold parity.
        assert_eq!(overlap(&a, &b), vec!["a.rs".to_string()]);
    }

    #[test]
    fn overlap_empty_when_disjoint() {
        let a = vec!["a.rs".to_string()];
        let b = vec!["b.rs".to_string()];
        assert!(overlap(&a, &b).is_empty());
    }

    #[test]
    fn overlap_result_is_sorted_and_deduped() {
        let a = vec!["z.rs".to_string(), "a.rs".to_string(), "a.rs".to_string()];
        let b = vec!["a.rs".to_string(), "z.rs".to_string()];
        assert_eq!(
            overlap(&a, &b),
            vec!["a.rs".to_string(), "z.rs".to_string()]
        );
    }

    #[test]
    fn detect_conflicts_names_undeclared_shared_file() {
        let mut reg = ChangesetRegistry::new();
        reg.insert(
            "runA/t1".to_string(),
            cs("runA/t1", &["shared.rs", "only1.rs"], 100),
        );
        let new = cs("runA/t2", &["shared.rs", "only2.rs"], 110);

        let events = detect_conflicts(&new, &reg, LEASE_TTL_SECS);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].task_key_a, "runA/t2");
        assert_eq!(events[0].task_key_b, "runA/t1");
        assert_eq!(events[0].overlapping_files, vec!["shared.rs".to_string()]);
        assert!(
            events[0].detail.contains("shared.rs"),
            "detail must name the file: {}",
            events[0].detail
        );
    }

    #[test]
    fn detect_conflicts_skips_self_merged_and_stale() {
        let mut reg = ChangesetRegistry::new();
        // self (same key) — must be skipped even though files overlap.
        reg.insert("runA/t2".to_string(), cs("runA/t2", &["shared.rs"], 100));
        // merged peer — excluded.
        let mut merged = cs("runA/t3", &["shared.rs"], 100);
        merged.merged = true;
        reg.insert("runA/t3".to_string(), merged);
        // stale peer (older than TTL relative to new.ts) — excluded.
        reg.insert("runA/t4".to_string(), cs("runA/t4", &["shared.rs"], 0));

        let new = cs("runA/t2", &["shared.rs"], LEASE_TTL_SECS + 100);
        let events = detect_conflicts(&new, &reg, LEASE_TTL_SECS);
        assert!(
            events.is_empty(),
            "self/merged/stale peers must not conflict"
        );
    }

    #[test]
    fn detect_conflicts_is_deterministic_across_multiple_peers() {
        let mut reg = ChangesetRegistry::new();
        reg.insert("runA/tb".to_string(), cs("runA/tb", &["shared.rs"], 100));
        reg.insert("runA/ta".to_string(), cs("runA/ta", &["shared.rs"], 100));
        let new = cs("runB/t9", &["shared.rs"], 120);
        let e1 = detect_conflicts(&new, &reg, LEASE_TTL_SECS);
        let e2 = detect_conflicts(&new, &reg, LEASE_TTL_SECS);
        assert_eq!(e1, e2);
        // BTreeMap key order -> ta before tb.
        assert_eq!(e1.len(), 2);
        assert_eq!(e1[0].task_key_b, "runA/ta");
        assert_eq!(e1[1].task_key_b, "runA/tb");
    }

    #[test]
    fn changeset_json_round_trips() {
        let c = cs("runA/t1", &["./Crates/Foo.rs"], 42);
        // files normalized on construction.
        assert_eq!(c.files, vec!["crates/foo.rs".to_string()]);
        let json = serde_json::to_string(&c).unwrap();
        let back: ActualChangeset = serde_json::from_str(&json).unwrap();
        assert_eq!(c, back);
    }
}
