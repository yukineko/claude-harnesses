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
//! ## Contract (asymmetric — this is the important nuance)
//!
//! Inputs that leave NOTHING to record — no worktree recorded on the task, an
//! unresolvable base, or a GENUINELY empty changed-file listing (git succeeded
//! and listed no file) — return [`OverlapCheck::NotRecorded`]: no changeset is
//! registered and no hold is set.
//!
//! A changed-file listing that `git` could NOT produce (every diff fallback
//! failed) is NOT an empty diff: the task's changes are unknown, so its overlap
//! with in-flight peers is undetermined. It is handled like an unchecked
//! overlap (backlog 79458be4): nothing is registered (there is no file set to
//! register) but the `<task_key>/runtime-overlap-unchecked` merge-hold is
//! enqueued ([`OverlapCheck::UncheckedHeld`]), or
//! [`OverlapCheck::HoldNotEnqueued`] is returned if that enqueue fails.
//!
//! Once a changeset IS submitted to the registry, the answer is three-valued
//! (backlog 08494f63):
//!
//! * the registry recorded and cross-checked it and found no overlap →
//!   [`OverlapCheck::NoOverlap`] (the only "no overlap" answer);
//! * it found a POSITIVE overlap → a merge-hold is enqueued
//!   ([`OverlapCheck::Held`]);
//! * the record/cross-check itself FAILED (lease lock not acquired, corrupt or
//!   unreadable registry, write error) → the overlap is UNCHECKED, not absent,
//!   so a merge-hold is enqueued for it too ([`OverlapCheck::UncheckedHeld`]);
//!   resolving that hold (`resolve-merge`) is the human's decision to merge
//!   without the check.
//!
//! A hold is a hold-for-review (NOT a hard run error): the merge is skipped
//! until the entry is resolved. If a needed hold could not be enqueued (the
//! review-surface append returned `Err`, e.g. the same lock is still
//! contended), the merge is NOT held: that is reported as
//! [`OverlapCheck::HoldNotEnqueued`], never as "held", and the caller names it
//! on stderr and exits non-zero.
//!
//! ## The hold's branch (backlog 38321ba7)
//!
//! The merge gate (`worktree::open_runtime_overlap_hold`) finds a hold by
//! `entry.branch == <branch being merged>`, so a hold written with an empty
//! branch can never be matched and holds nothing. The branch is therefore the
//! task's recorded `branch`, or — when none was recorded — the branch checked
//! out in the task worktree (`git symbolic-ref --short -q HEAD`). If neither
//! yields a branch (detached HEAD, git failure), a needed hold is NOT written
//! and the result is [`OverlapCheck::HoldNotEnqueued`] (state set exits
//! non-zero); an empty-branch hold is never written.

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
/// `git diff --name-only <base>...HEAD` with the same fallback cascade as the
/// diff-risk hook. Three answers (backlog 79458be4):
///
/// * `Absent` — the worktree directory does not exist (nothing to inspect);
/// * `Undetermined(why)` — EVERY diff fallback failed: the changed set is
///   unknown, which is NOT the same as empty;
/// * `Known(files)` — git listed the files; `files` may be genuinely empty.
fn changed_files(worktree: &Path, base: &str) -> ChangedFiles {
    if !worktree.exists() {
        return ChangedFiles::Absent;
    }
    let spec = format!("{base}...HEAD");
    let out = crate::worktree::git(worktree, &["diff", "--name-only", &spec])
        .or_else(|_| crate::worktree::git(worktree, &["diff", "--name-only", base]))
        .or_else(|_| crate::worktree::git(worktree, &["diff", "--name-only", "HEAD"]));
    match out {
        Ok(out) => ChangedFiles::Known(
            out.lines()
                .map(|l| l.trim())
                .filter(|l| !l.is_empty())
                .map(|l| l.to_string())
                .collect(),
        ),
        Err(e) => ChangedFiles::Undetermined(format!("{e:#}")),
    }
}

/// The result of [`changed_files`]. See its docs.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ChangedFiles {
    Absent,
    Undetermined(String),
    Known(Vec<String>),
}

/// The branch a hold for `task` must carry so the merge gate can match it
/// (backlog 38321ba7): the task's recorded branch, else the branch checked out
/// in its worktree. `None` when neither is available (no recorded branch and a
/// detached HEAD / git failure) — the caller must then NOT write a hold.
fn hold_branch(task: &TaskState, worktree: &Path) -> Option<String> {
    if let Some(b) = task.branch.as_deref().map(str::trim) {
        if !b.is_empty() {
            return Some(b.to_string());
        }
    }
    crate::worktree::git(worktree, &["symbolic-ref", "--short", "-q", "HEAD"])
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Enqueue `entry` (its `branch` set from `branch`) as an open merge-hold.
/// `on_ok` is the answer when it lands. A missing branch or an append error
/// both mean NO matchable hold exists, so both return `HoldNotEnqueued`.
fn enqueue_hold(
    repo: &Path,
    mut entry: MergeConflictEntry,
    branch: Option<&str>,
    unchecked: bool,
    context: &str,
    on_ok: OverlapCheck,
) -> OverlapCheck {
    let Some(branch) = branch else {
        return OverlapCheck::HoldNotEnqueued {
            unchecked,
            why: format!(
                "{context}; hold NOT written: the task has no recorded branch and none could be \
                 derived from its worktree, so the merge gate could never match the hold"
            ),
        };
    };
    entry.branch = branch.to_string();
    match overwatch::store::append_merge_conflict(repo, &entry) {
        Ok(()) => on_ok,
        Err(e) => OverlapCheck::HoldNotEnqueued {
            unchecked,
            why: format!("{context}; hold enqueue failed: {e:#}"),
        },
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

/// What [`record_and_check_actual_overlap`] established. See the module docs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OverlapCheck {
    /// Nothing was submitted to the registry (no worktree / base, or a
    /// genuinely empty changed-file listing), so no overlap check ran and no
    /// hold was set. NOT returned when git could not list the changed files —
    /// that is undetermined and holds the merge (backlog 79458be4).
    NotRecorded,
    /// Recorded and cross-checked: no overlap with any in-flight peer.
    NoOverlap,
    /// A positive overlap was detected and the merge-hold was enqueued.
    Held,
    /// The overlap check could NOT run (`why`); a merge-hold was enqueued so
    /// the merge does not proceed unchecked.
    UncheckedHeld(String),
    /// A hold was needed (positive overlap, or `unchecked` = the check could
    /// not run) but enqueueing it FAILED (`why`): the merge is NOT held.
    HoldNotEnqueued { unchecked: bool, why: String },
}

/// Record this task's actual changeset and, on a detected mid-flight overlap
/// with another in-flight worktree — or when the overlap check itself could
/// not run — set a merge-hold (decision A; backlog 08494f63). See
/// [`OverlapCheck`] and the module docs for the full contract.
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
) -> OverlapCheck {
    let worktree = match task.worktree.as_deref() {
        Some(w) => Path::new(w),
        None => return OverlapCheck::NotRecorded,
    };
    let base = match frozen_base(worktree, &cfg.default_branch) {
        Some(b) => b,
        None => return OverlapCheck::NotRecorded,
    };
    let repo = crate::worktree::toplevel(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    let task_key = format!("{run_id}/{}", task.id);
    let branch = hold_branch(task, worktree);
    let unchecked_entry = |files: Vec<String>, diff_theirs: String, why: &str| {
        MergeConflictEntry {
            conflict_id: format!("{task_key}/runtime-overlap-unchecked"),
            origin: ConflictOrigin::RuntimeOverlap,
            run_id: run_id.to_string(),
            // Set by `enqueue_hold` (never written empty).
            branch: String::new(),
            default_branch: cfg.default_branch.clone(),
            base_ref: base.clone(),
            conflicted_files: files,
            diff_theirs,
            diff_ours: format!(
                "mid-flight overlap check NOT performed (undetermined, not absent): {why}"
            ),
            ts: now,
        }
    };

    let files = match changed_files(worktree, &base) {
        ChangedFiles::Known(f) if !f.is_empty() => f,
        // Genuinely nothing to record.
        ChangedFiles::Known(_) | ChangedFiles::Absent => return OverlapCheck::NotRecorded,
        // git could not list the changes: the overlap is undetermined, not
        // absent — hold the merge like an unchecked overlap (backlog 79458be4).
        ChangedFiles::Undetermined(e) => {
            let why = format!("git could not list the task's changed files: {e}");
            let entry = unchecked_entry(Vec::new(), String::new(), &why);
            return enqueue_hold(
                &repo,
                entry,
                branch.as_deref(),
                true,
                &format!("overlap check failed: {why}"),
                OverlapCheck::UncheckedHeld(why.clone()),
            );
        }
    };

    let changeset = ActualChangeset::new(
        task_key.clone(),
        run_id.to_string(),
        session_id.to_string(),
        branch.clone().unwrap_or_default(),
        base.clone(),
        head_sha(worktree),
        &files,
        now,
    );
    let diff_theirs = || truncate_diff(&diff_text(worktree, &changeset.base_ref), DIFF_BYTE_CAP);

    // Shared-registry RMW + cross-check. An `Err` (lock not acquired, corrupt
    // registry, write failure) means the overlap was NOT checked — it is
    // undetermined, not "no overlap" — so it holds the merge like a positive
    // detection does (backlog 08494f63).
    let events = match overwatch::store::record_changeset_and_detect(&repo, &changeset) {
        Ok(evs) => evs,
        Err(e) => {
            let why = format!("{e:#}");
            // Every file this task changed: none of them was cross-checked.
            let entry = unchecked_entry(files.clone(), diff_theirs(), &why);
            return enqueue_hold(
                &repo,
                entry,
                branch.as_deref(),
                true,
                &format!("overlap check failed: {why}"),
                OverlapCheck::UncheckedHeld(why.clone()),
            );
        }
    };
    if events.is_empty() {
        return OverlapCheck::NoOverlap;
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
        // Set by `enqueue_hold` (never written empty).
        branch: String::new(),
        default_branch: cfg.default_branch.clone(),
        base_ref: base.clone(),
        conflicted_files: overlapping.into_iter().collect(),
        // "theirs" = this task's actual diff (byte-bounded); "ours" = a note of
        // the in-flight peer(s) it overlaps (their worktrees are elsewhere, so a
        // full diff isn't available here — the resolver has enough to act).
        diff_theirs: diff_theirs(),
        diff_ours: format!(
            "mid-flight overlap with in-flight peer task(s): {}",
            peers.join(", ")
        ),
        ts: now,
    };
    // Idempotent per conflict_id (append_merge_conflict re-checks membership).
    // A failed enqueue means NO hold is on the review surface, so the merge is
    // not held: report that, never "held" (backlog 452f3705). The
    // runtime_conflicts event itself was already recorded by the call above.
    // A hold without a determinable branch is likewise not a hold (38321ba7).
    enqueue_hold(
        &repo,
        entry,
        branch.as_deref(),
        false,
        "overlap detected",
        OverlapCheck::Held,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_op_when_worktree_absent() {
        let cfg = Config::without_config_file();
        let task = TaskState {
            id: "t1".to_string(),
            ..Default::default()
        };
        // No worktree recorded → nothing submitted, no panic, no hold.
        assert_eq!(
            record_and_check_actual_overlap(&cfg, Path::new("/tmp"), "run-x", &task, 0, "sess"),
            OverlapCheck::NotRecorded
        );
        // A worktree path that does not exist → also not recorded.
        let task = TaskState {
            id: "t1".to_string(),
            worktree: Some("/definitely/not/a/real/worktree/xyzzy".to_string()),
            ..Default::default()
        };
        assert_eq!(
            record_and_check_actual_overlap(&cfg, Path::new("/tmp"), "run-x", &task, 0, "sess"),
            OverlapCheck::NotRecorded
        );
    }

    /// Pin `HOME` (the overwatch storage root) to `home` for the closure,
    /// serialized with every other HOME-mutating test in this crate.
    fn with_home<R>(home: &Path, f: impl FnOnce() -> R) -> R {
        let _g = crate::env_lock::HOME_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let prev = std::env::var_os("HOME");
        std::env::set_var("HOME", home);
        let out = f();
        match prev {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        out
    }

    /// A repo on `main` with one commit, then branch `feat` checked out with a
    /// committed `feat.txt` — so the task's actual diff vs main is non-empty.
    fn repo_with_feat_branch(dir: &Path) {
        let g = |args: &[&str]| crate::worktree::git(dir, args).unwrap();
        g(&["init", "-b", "main"]);
        g(&["config", "user.email", "test@example.com"]);
        g(&["config", "user.name", "Test"]);
        std::fs::write(dir.join("base.txt"), "base\n").unwrap();
        g(&["add", "."]);
        g(&["commit", "-m", "init"]);
        g(&["checkout", "-b", "feat"]);
        std::fs::write(dir.join("feat.txt"), "feat\n").unwrap();
        g(&["add", "."]);
        g(&["commit", "-m", "feat"]);
    }

    /// 08494f63: when the overlap check could NOT run (the registry RMW
    /// errored — here a corrupt `active_changesets.json`), the task's merge
    /// must be HELD, not waved through as "no overlap". Observed via the same
    /// open-hold ledger the merge gate reads.
    #[test]
    fn unchecked_overlap_holds_the_merge() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        repo_with_feat_branch(&repo);
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let cfg = Config::without_config_file();
        let task = TaskState {
            id: "t1".to_string(),
            worktree: Some(repo.to_string_lossy().into_owned()),
            branch: Some("feat".to_string()),
            ..Default::default()
        };
        let held = with_home(&home, || {
            let reg = overwatch::store::active_changesets_path(&repo).unwrap();
            std::fs::create_dir_all(reg.parent().unwrap()).unwrap();
            std::fs::write(&reg, "{not json").unwrap();
            let r = record_and_check_actual_overlap(&cfg, &repo, "run-x", &task, 1, "sess");
            assert!(
                matches!(r, OverlapCheck::UncheckedHeld(_)),
                "corrupt registry must be reported as an unchecked, HELD overlap; got {r:?}"
            );
            let scan = overwatch::store::scan_open_merge_conflicts(&repo).unwrap();
            match scan.open {
                harness_core::verdict::Determination::Known(open) => open.into_iter().any(|e| {
                    e.branch == "feat"
                        && matches!(
                            e.origin,
                            overwatch::merge_conflict::ConflictOrigin::RuntimeOverlap
                        )
                }),
                harness_core::verdict::Determination::Undetermined(why) => {
                    panic!("hold ledger unreadable: {}", why.as_str())
                }
            }
        });
        assert!(
            held,
            "an UNCHECKED overlap (registry corrupt) must enqueue a merge-hold for 'feat'; \
             none was found, so the merge would proceed unchecked"
        );
    }

    /// 08494f63 + 452f3705: with the overwatch lease lock held by a live peer
    /// for the whole deadline, neither the record nor the hold enqueue can run.
    /// That must come back as `HoldNotEnqueued { unchecked: true }` (the caller
    /// then exits non-zero), never `NoOverlap` / `Held`.
    #[test]
    fn contended_lock_reports_hold_not_enqueued() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        repo_with_feat_branch(&repo);
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let cfg = Config::without_config_file();
        let task = TaskState {
            id: "t1".to_string(),
            worktree: Some(repo.to_string_lossy().into_owned()),
            branch: Some("feat".to_string()),
            ..Default::default()
        };
        let out = with_home(&home, || {
            let lock = overwatch::store::leases_path(&repo)
                .unwrap()
                .with_extension("lock");
            std::fs::create_dir_all(lock.parent().unwrap()).unwrap();
            std::fs::write(
                &lock,
                format!(
                    "{{\"pid\":{},\"acquired_at\":{}}}",
                    std::process::id(),
                    overwatch::store::now()
                ),
            )
            .unwrap();
            let out = record_and_check_actual_overlap(&cfg, &repo, "run-x", &task, 1, "sess");
            let _ = std::fs::remove_file(&lock);
            out
        });
        assert!(
            matches!(out, OverlapCheck::HoldNotEnqueued { unchecked: true, .. }),
            "a contended lock must report an UNCHECKED overlap whose hold was not enqueued, got {out:?}"
        );
    }

    #[test]
    fn changed_files_absent_on_missing_worktree() {
        assert_eq!(
            changed_files(Path::new("/definitely/not/a/dir/xyzzy"), "main"),
            ChangedFiles::Absent
        );
    }

    /// 79458be4: a genuinely empty diff is `Known(empty)` and records nothing
    /// (no hold) — distinct from a git failure.
    #[test]
    fn genuinely_empty_diff_records_nothing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        repo_with_feat_branch(&repo);
        crate::worktree::git(&repo, &["checkout", "-q", "main"]).unwrap();
        crate::worktree::git(&repo, &["checkout", "-q", "-b", "empty"]).unwrap();
        assert_eq!(
            changed_files(&repo, "main"),
            ChangedFiles::Known(Vec::new())
        );
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let cfg = Config::without_config_file();
        let task = TaskState {
            id: "t1".to_string(),
            worktree: Some(repo.to_string_lossy().into_owned()),
            branch: Some("empty".to_string()),
            ..Default::default()
        };
        let (r, has_hold) = with_home(&home, || {
            let r = record_and_check_actual_overlap(&cfg, &repo, "run-x", &task, 1, "sess");
            let p = overwatch::store::merge_conflicts_path(&repo).unwrap();
            (r, p.exists())
        });
        assert_eq!(r, OverlapCheck::NotRecorded);
        assert!(!has_hold, "a genuinely empty diff must not write a hold");
    }

    /// 38321ba7: with no recorded branch the hold branch is derived from the
    /// worktree; a detached HEAD yields none (so no hold may be written).
    #[test]
    fn hold_branch_derives_from_worktree_or_none() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        repo_with_feat_branch(&repo);
        let mut task = TaskState {
            id: "t1".to_string(),
            ..Default::default()
        };
        assert_eq!(hold_branch(&task, &repo).as_deref(), Some("feat"));
        task.branch = Some("recorded".to_string());
        assert_eq!(hold_branch(&task, &repo).as_deref(), Some("recorded"));
        task.branch = Some(String::new());
        crate::worktree::git(&repo, &["checkout", "-q", "--detach"]).unwrap();
        assert_eq!(hold_branch(&task, &repo), None);
    }

    /// 38321ba7: an unchecked overlap for a task with no determinable branch
    /// (detached HEAD, none recorded) must NOT write a hold and must report
    /// `HoldNotEnqueued` — never an empty-branch hold reported as held.
    #[test]
    fn no_determinable_branch_is_hold_not_enqueued() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        repo_with_feat_branch(&repo);
        crate::worktree::git(&repo, &["checkout", "-q", "--detach"]).unwrap();
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let cfg = Config::without_config_file();
        let task = TaskState {
            id: "t1".to_string(),
            worktree: Some(repo.to_string_lossy().into_owned()),
            ..Default::default()
        };
        let (r, holds) = with_home(&home, || {
            let reg = overwatch::store::active_changesets_path(&repo).unwrap();
            std::fs::create_dir_all(reg.parent().unwrap()).unwrap();
            std::fs::write(&reg, "{not json").unwrap();
            let r = record_and_check_actual_overlap(&cfg, &repo, "run-x", &task, 1, "sess");
            let p = overwatch::store::merge_conflicts_path(&repo).unwrap();
            (r, std::fs::read_to_string(p).unwrap_or_default())
        });
        assert!(
            matches!(
                r,
                OverlapCheck::HoldNotEnqueued {
                    unchecked: true,
                    ..
                }
            ),
            "no determinable branch must be HoldNotEnqueued, got {r:?}"
        );
        assert!(holds.is_empty(), "no hold may be written: {holds}");
    }
}
