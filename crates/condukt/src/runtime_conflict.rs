//! Mid-flight runtime-conflict detection hook (design 625aa170 — decision A).
//!
//! A sibling of the diff-risk hook ([`crate::diffrisk_record`]): it fires at the
//! same `state set --status done` edge (post-worker, pre-merge), when the task's
//! worktree is still on disk. It computes the task's ACTUAL changed-file set
//! (`git diff <base>...HEAD --name-only`, three-dot merge-base, fail-soft
//! cascade) and records it into the overwatch project-global registry
//! (`active_changesets.json`). Recording cross-checks the new changeset against
//! every OTHER in-flight worktree and, on a genuine path overlap, does TWO
//! things (decision A: GATE the merge, don't just annotate):
//!
//! 1. records the `runtime_conflicts.jsonl` event(s) (done inside
//!    [`overwatch::store::record_changeset_and_detect`]); and
//! 2. sets a **merge-hold** on this task by enqueuing an open
//!    [`overwatch::merge_conflict::MergeConflictEntry`] (origin
//!    `RuntimeOverlap`) into the SAME consensus review surface a real merge
//!    conflict uses. The condukt merge path checks this hold and SKIPS the
//!    merge; `resolve-merge` clears it (records a resolution → the entry leaves
//!    the open set) then proceeds.
//!
//! ## Fail-soft contract (asymmetric — this is the important nuance)
//!
//! The DETECTION/compute side is fully fail-soft: a missing/absent worktree, a
//! `git` failure, an empty diff, a lock timeout, or a write error all degrade to
//! "no overlap detected" (`false`), never holding a merge on a compute error and
//! never changing the exit code. The HOLD itself, once a POSITIVE overlap is
//! detected, is a real block (a hold-for-review, NOT a hard run error): the
//! merge is skipped until the entry is resolved. Returns `true` iff a hold was
//! set (a real overlap was detected), for the caller to log / observe.

use crate::config::Config;
use crate::state::TaskState;
use overwatch::changeset::ActualChangeset;
use overwatch::merge_conflict::{truncate_diff, ConflictOrigin, MergeConflictEntry, DIFF_BYTE_CAP};
use std::collections::BTreeSet;
use std::path::Path;

/// The frozen three-dot merge-base SHA the actual diff is taken against, so a
/// later-moving default branch does not shift the comparison. Falls back to the
/// default-branch NAME when the merge-base can't be resolved (still a valid
/// three-dot spec, just not frozen). Best-effort; `None` only when neither works.
fn frozen_base(worktree: &Path, default_branch: &str) -> Option<String> {
    if let Ok(sha) = crate::worktree::git(worktree, &["merge-base", "HEAD", default_branch]) {
        let sha = sha.trim().to_string();
        if !sha.is_empty() {
            return Some(sha);
        }
    }
    // Fall back to the branch name (unfrozen but functional).
    if crate::worktree::git(worktree, &["rev-parse", "--verify", default_branch]).is_ok() {
        return Some(default_branch.to_string());
    }
    None
}

/// The worktree HEAD SHA (best-effort; empty string when unavailable).
fn head_sha(worktree: &Path) -> String {
    crate::worktree::git(worktree, &["rev-parse", "HEAD"])
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// The ACTUAL changed files (repo-relative) of the worktree vs `base`, via
/// `git diff --name-only <base>...HEAD` with the same fail-soft cascade as the
/// diff-risk hook. `None` when there is no worktree, git fails, or the set is
/// empty — every one of which the caller degrades to "record nothing".
fn changed_files(worktree: &Path, base: &str) -> Option<Vec<String>> {
    if !worktree.exists() {
        return None;
    }
    let spec = format!("{base}...HEAD");
    let out = crate::worktree::git(worktree, &["diff", "--name-only", &spec])
        .or_else(|_| crate::worktree::git(worktree, &["diff", "--name-only", base]))
        .or_else(|_| crate::worktree::git(worktree, &["diff", "--name-only", "HEAD"]))
        .ok()?;
    let files: Vec<String> = out
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect();
    if files.is_empty() {
        None
    } else {
        Some(files)
    }
}

/// The worktree's unified diff text vs `base` (byte-bounded by the caller), for
/// the review entry's `diff_theirs`. Best-effort; empty when unavailable.
fn diff_text(worktree: &Path, base: &str) -> String {
    let spec = format!("{base}...HEAD");
    crate::worktree::git(worktree, &["diff", &spec])
        .or_else(|_| crate::worktree::git(worktree, &["diff", base]))
        .or_else(|_| crate::worktree::git(worktree, &["diff", "HEAD"]))
        .unwrap_or_default()
}

/// Record this task's actual changeset and, on a detected mid-flight overlap
/// with another in-flight worktree, set a merge-hold (decision A). Returns
/// `true` iff a hold was set. Fully fail-soft on the compute side (see module
/// docs); a positive detection is the only path that holds.
///
/// `cwd` is condukt's invocation cwd (the MAIN repo), NOT the task worktree:
/// the overwatch registry is keyed by the project (repo-root → project-key), so
/// every worktree of the same project shares one registry. Mirrors how
/// `diffrisk_record` resolves the repo top-level for `append_violation`.
pub(crate) fn record_and_check_actual_overlap(
    cfg: &Config,
    cwd: &Path,
    run_id: &str,
    task: &TaskState,
    now: i64,
    session_id: &str,
) -> bool {
    let worktree = match task.worktree.as_deref() {
        Some(w) => Path::new(w),
        None => return false,
    };
    let base = match frozen_base(worktree, &cfg.default_branch) {
        Some(b) => b,
        None => return false,
    };
    let files = match changed_files(worktree, &base) {
        Some(f) => f,
        None => return false,
    };

    let repo = crate::worktree::toplevel(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    let task_key = format!("{run_id}/{}", task.id);
    let branch = task.branch.clone().unwrap_or_default();
    let changeset = ActualChangeset::new(
        task_key.clone(),
        run_id.to_string(),
        session_id.to_string(),
        branch.clone(),
        base.clone(),
        head_sha(worktree),
        &files,
        now,
    );

    // Shared-registry RMW + cross-check. Fail-soft: an Err (lock/write failure)
    // degrades to "no overlap" — a COMPUTE error never holds a merge.
    let events = match overwatch::store::record_changeset_and_detect(&repo, &changeset) {
        Ok(evs) => evs,
        Err(e) => {
            eprintln!("condukt: runtime-conflict detection failed (continuing, no hold): {e}");
            return false;
        }
    };
    if events.is_empty() {
        return false;
    }

    // POSITIVE detection → set the merge-hold by enqueuing an open
    // RuntimeOverlap entry into the SAME review surface a real conflict uses.
    let overlapping: BTreeSet<String> = events
        .iter()
        .flat_map(|e| e.overlapping_files.iter().cloned())
        .collect();
    let peers: Vec<String> = {
        let mut v: BTreeSet<String> = events.iter().map(|e| e.task_key_b.clone()).collect();
        v.remove(&task_key);
        v.into_iter().collect()
    };
    let entry = MergeConflictEntry {
        conflict_id: format!("{task_key}/runtime-overlap"),
        origin: ConflictOrigin::RuntimeOverlap,
        run_id: run_id.to_string(),
        branch,
        default_branch: cfg.default_branch.clone(),
        base_ref: base,
        conflicted_files: overlapping.into_iter().collect(),
        // "theirs" = this task's actual diff (byte-bounded); "ours" = a note of
        // the in-flight peer(s) it overlaps (their worktrees are elsewhere, so a
        // full diff isn't available here — the resolver has enough to act).
        diff_theirs: truncate_diff(&diff_text(worktree, &changeset.base_ref), DIFF_BYTE_CAP),
        diff_ours: format!(
            "mid-flight overlap with in-flight peer task(s): {}",
            peers.join(", ")
        ),
        ts: now,
    };
    // Idempotent per conflict_id (append_merge_conflict re-checks membership).
    // Fail-soft on the write: a failed enqueue is logged; the runtime_conflicts
    // event is already recorded, so the overlap is not lost.
    if let Err(e) = overwatch::store::append_merge_conflict(&repo, &entry) {
        eprintln!("condukt: could not enqueue runtime-overlap merge-hold (continuing): {e}");
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_op_when_worktree_absent() {
        let cfg = Config::load();
        let task = TaskState {
            id: "t1".to_string(),
            ..Default::default()
        };
        // No worktree recorded → immediate false, no panic, no hold.
        assert!(!record_and_check_actual_overlap(
            &cfg,
            Path::new("/tmp"),
            "run-x",
            &task,
            0,
            "sess"
        ));
        // A worktree path that does not exist → also false (changed_files None).
        let task = TaskState {
            id: "t1".to_string(),
            worktree: Some("/definitely/not/a/real/worktree/xyzzy".to_string()),
            ..Default::default()
        };
        assert!(!record_and_check_actual_overlap(
            &cfg,
            Path::new("/tmp"),
            "run-x",
            &task,
            0,
            "sess"
        ));
    }

    #[test]
    fn changed_files_none_on_missing_worktree() {
        assert!(changed_files(Path::new("/definitely/not/a/dir/xyzzy"), "main").is_none());
    }
}
