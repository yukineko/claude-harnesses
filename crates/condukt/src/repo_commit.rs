//! The in-process site for the PRIMARY working tree's staging+commit.
//!
//! condukt has two execution shapes that implement work directly in the primary
//! working tree instead of a per-task worktree: single-worktree mode
//! (`config.single_worktree`) and the small-task fast path. Both used to stage
//! and commit from the `/condukt` skill's own shell (`git add <paths> && git
//! commit`), so there was NO in-process site that could hold
//! [`crate::lock::REPO_PRIMARY_LOCK_KEY`] — that path was serialized only by the
//! coarse upstream `/flow` backlog run.lock. Two sessions sharing ONE index and
//! working tree is the one conflict git cannot resolve by merging: branch
//! isolation and merge-conflict resolution simply do not apply, and the loser's
//! staged content silently lands inside the winner's commit.
//!
//! This module moves that read-modify-write into condukt so it can be
//! serialized like every other primary-repo mutator (`worktree::merge`,
//! `git worktree prune`).
//!
//! Cannot-determine resolves to the RESTRICTIVE side throughout: an unheld lock,
//! an empty path set, foreign content already staged in the shared index, or a
//! failing `git` all REFUSE (error, no commit). None of them degrade into
//! "no conflict, proceed".
use anyhow::{bail, Context, Result};
use std::path::Path;
use std::time::Duration;

use crate::config::Config;
use crate::lock::{self, RunLock};
use crate::worktree;

/// Test-only hook: milliseconds to sleep between `git add` and `git commit`,
/// i.e. INSIDE the repo-primary critical section. It widens the window in which
/// a concurrent process could interleave its own staging into the shared index,
/// so the isolation regression test
/// (`tests/repo_commit_index_isolation.rs`) observes the race deterministically
/// instead of only under lucky timing. It changes no decision: every check and
/// every verdict below is identical with or without it.
pub const RACE_DELAY_ENV: &str = "CONDUKT_REPO_COMMIT_RACE_DELAY_MS";

/// Reject a pathspec we cannot treat as one concrete, task-owned file.
///
/// The whole point of this command is that the primary tree is SHARED, so the
/// commit must carry only this task's files. Anything that could widen the set
/// beyond what the caller literally named — `.`, `..`, a glob, a leading `-`
/// (an option) or `:` (git pathspec magic like `:/` = whole repo) — is refused
/// rather than guessed at.
fn validate_path(p: &str) -> Result<()> {
    let t = p.trim();
    if t.is_empty() {
        bail!("empty --path is not a file to commit");
    }
    if t == "." || t == ".." || t == "./" {
        bail!("--path {p:?} would widen the commit to the whole tree; name explicit files");
    }
    if t.starts_with('-') {
        bail!("--path {p:?} looks like an option, not a path");
    }
    if t.starts_with(':') {
        bail!("--path {p:?} uses git pathspec magic; name explicit files instead");
    }
    if t.contains('*') || t.contains('?') || t.contains('[') {
        bail!("--path {p:?} is a glob; name explicit files so the commit stays task-scoped");
    }
    Ok(())
}

/// Paths already staged in the index (the `X` column of `git status --porcelain`
/// is neither space nor `?`). Works on an unborn branch too, unlike
/// `git diff --cached`.
fn staged_paths(repo: &Path) -> Result<Vec<String>> {
    let out = worktree::git(repo, &["status", "--porcelain"])
        .context("could not read the shared index state (git status --porcelain)")?;
    Ok(out
        .lines()
        .filter(|l| l.len() > 3)
        .filter(|l| {
            let x = l.as_bytes()[0] as char;
            x != ' ' && x != '?'
        })
        .map(|l| l[3..].to_string())
        .collect())
}

fn race_delay() -> Option<Duration> {
    std::env::var(RACE_DELAY_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|ms| *ms > 0)
        .map(Duration::from_millis)
}

/// Stage `paths` and commit them in the primary working tree, holding the
/// repo-scoped primary lock across the WHOLE read-modify-write. Returns the new
/// commit sha.
pub fn commit(cfg: &Config, repo: &Path, paths: &[String], message: &str) -> Result<String> {
    if paths.is_empty() {
        bail!(
            "refusing to commit with no --path: the primary working tree is shared, \
             so the files belonging to this task must be named explicitly"
        );
    }
    for p in paths {
        validate_path(p)?;
    }
    if message.trim().is_empty() {
        bail!("refusing to commit with an empty message");
    }

    // Serialize the WHOLE read-modify-write against every other primary-repo
    // mutator (a peer `repo commit`, `worktree::merge`, `git worktree prune`) on
    // the one repo-scoped lock.
    //
    // `acquire_or_skip` is FALLIBLE on purpose: the fail-soft `RunLock::acquire`
    // this crate used to expose handed back an unheld guard on timeout/IO error,
    // which would let two timed-out writers both stage into the shared index,
    // i.e. exactly the defect this command exists to close. It has since been
    // deleted outright (v0.7.91), so no primary-repo mutator can regress to it.
    // An unheld lock is cannot-determine, and cannot-determine resolves to the
    // restrictive side: refuse, commit nothing.
    let Some(_repo_lock) = RunLock::acquire_or_skip(cfg, repo, lock::REPO_PRIMARY_LOCK_KEY) else {
        bail!(
            "could not acquire the repo-primary lock for {} within {:?}; \
             refusing to stage/commit unlocked (a concurrent condukt execution \
             could interleave its content into this commit)",
            repo.display(),
            RunLock::DEADLINE
        );
    };
    commit_locked(repo, paths, message)
}

/// The critical section. Split out so the lock acquisition sits directly above
/// the read-modify-write it protects.
fn commit_locked(repo: &Path, paths: &[String], message: &str) -> Result<String> {
    // Foreign content already in the shared index is indistinguishable from a
    // peer condukt process staging mid-flight. We cannot determine whose it is,
    // so we refuse instead of sweeping it into this task's commit.
    let staged = staged_paths(repo)?;
    if !staged.is_empty() {
        bail!(
            "the shared index already has staged content this command did not stage ({}); \
             refusing to commit — it would land in this task's commit. \
             Commit or reset it first.",
            staged.join(", ")
        );
    }

    let mut args: Vec<&str> = vec!["add", "--"];
    args.extend(paths.iter().map(|s| s.as_str()));
    worktree::git(repo, &args).context("staging this task's paths failed")?;

    if let Some(d) = race_delay() {
        std::thread::sleep(d);
    }

    worktree::git(repo, &["commit", "-m", message])
        .context("committing this task's staged paths failed")?;

    worktree::git(repo, &["rev-parse", "HEAD"]).context("could not read the new commit sha")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widening_pathspecs_are_refused() {
        for bad in [
            "", "   ", ".", "..", "./", "-A", "--all", ":/", "src/*.rs", "a?.rs", "a[0].rs",
        ] {
            assert!(
                validate_path(bad).is_err(),
                "{bad:?} must be refused: it can widen the commit beyond this task"
            );
        }
    }

    #[test]
    fn concrete_paths_are_accepted() {
        for ok in ["a.txt", "crates/condukt/src/main.rs", "dir/sub/file.rs"] {
            assert!(validate_path(ok).is_ok(), "{ok:?} must be accepted");
        }
    }

    #[test]
    fn race_delay_is_off_unless_set_to_a_positive_number() {
        // Guard the test-only hook: a missing/invalid/zero value must be a no-op
        // so production never sleeps inside the critical section.
        assert_eq!(RACE_DELAY_ENV, "CONDUKT_REPO_COMMIT_RACE_DELAY_MS");
        std::env::remove_var(RACE_DELAY_ENV);
        assert!(race_delay().is_none());
        std::env::set_var(RACE_DELAY_ENV, "0");
        assert!(race_delay().is_none());
        std::env::set_var(RACE_DELAY_ENV, "nonsense");
        assert!(race_delay().is_none());
        std::env::set_var(RACE_DELAY_ENV, "5");
        assert_eq!(race_delay(), Some(Duration::from_millis(5)));
        std::env::remove_var(RACE_DELAY_ENV);
    }
}
