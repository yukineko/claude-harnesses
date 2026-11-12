//! Per-run cap on LIVE worktrees (`Config::max_parallel`), enforced at
//! `condukt worktree create --run <id>`.
//!
//! The cap is a check-then-act sequence (count the run's live worktrees, then
//! create one), so it is only a cap if no other create for the same run can
//! interleave between the two steps (backlog 32b69400: two concurrent creates
//! under `max_parallel = 1` both observed zero live worktrees and both
//! created one). The caller therefore holds [`acquire_run_cap_lock`] across
//! [`enforce_run_cap`] AND the worktree creation; a lock that cannot be
//! acquired refuses the create.

use anyhow::{bail, Context, Result};
use harness_core::verdict::Required;
use std::path::Path;

use crate::config::Config;
use crate::lock::RunLock;
use crate::worktree;

/// Prefix of the reserved lock key that serializes the cap check + create of
/// one run. The run id follows it (sanitised by the lock-path composition, so
/// two run ids that sanitise to the same name share a lock: over-serialization,
/// the restrictive side). It never names a real run, so it cannot collide with
/// a run-state lock.
pub const RUN_CAP_LOCK_PREFIX: &str = "__run_cap__.";

/// Acquire the per-run cap lock: `<state_dir>/<project-key of the MAIN
/// worktree root>/__run_cap__.<run>.lock`. Keyed by the main worktree root so
/// a create issued from a linked worktree and one issued from the main tree of
/// the same repo contend ONE file. The lock lives in condukt's state dir, never
/// in the repo.
///
/// Hold the returned guard across [`enforce_run_cap`] and the worktree
/// creation (which takes the repo-primary lock inside; lock order is always
/// run-cap -> repo-primary, and nothing takes them in the other order).
///
/// Fail-closed (CLAUDE.md §3): the main worktree root cannot be determined, or
/// the lock cannot be acquired within [`RunLock::DEADLINE`] (live contention or
/// an I/O error) -> `Err`, and the caller refuses the create. There is no
/// unlocked fallback.
pub fn acquire_run_cap_lock(cfg: &Config, repo: &Path, run: &str) -> Result<RunLock> {
    let root = match harness_core::projkey::main_worktree_root(repo).require() {
        Required::Determined(root) => root,
        Required::Blocked(v) => bail!(
            "max-parallel-undetermined: cannot determine the main worktree root of {} ({}); \
             the run-cap lock for run '{run}' cannot be addressed, refusing to create",
            repo.display(),
            v.as_str()
        ),
    };
    let key = format!("{RUN_CAP_LOCK_PREFIX}{run}");
    match RunLock::acquire_or_skip_in_project(cfg, &root, &key) {
        Some(guard) => Ok(guard),
        None => bail!(
            "max-parallel-undetermined: could not acquire the run-cap lock for run '{run}' \
             within {:?} (another create for this run may be in flight, or the lock file \
             under {} is unusable); refusing to create (cap {})",
            RunLock::DEADLINE,
            cfg.state_dir.display(),
            cfg.max_parallel
        ),
    }
}

/// Refuse when run `run` already has `max_parallel` or more LIVE worktrees.
///
/// The count is only meaningful while the caller holds
/// [`acquire_run_cap_lock`] for `run` and keeps holding it until the new
/// worktree exists; called without it, two creates can both pass.
///
/// A worktree is LIVE for `run` when it is registered (`git worktree list`),
/// is not the primary working tree, and belongs to `run` by EITHER witness:
///
/// - **branch**: its branch is namespaced under `run` (`.../<run>/<last>`, as
///   `worktree::create_namespaced` cuts it), or
/// - **path**: it sits directly in `worktree_base` under a directory named
///   `<run>-...` (the `<worktree_base>/<run>-<topic>` layout that
///   `create_namespaced` uses). This witness keeps a worktree of the run
///   counted after its HEAD was detached (`git checkout --detach`, branch
///   `None`) or switched to an un-namespaced branch: the branch is gone, the
///   path is not (backlog df055fa4).
///
/// A worktree matching both witnesses counts once. Every failure direction
/// (CLAUDE.md §3) is toward counting MORE, never fewer:
///
/// - the live set cannot be listed (git fails) -> refuse (fail-closed);
/// - a directory name in `worktree_base` that is not valid UTF-8 cannot be
///   compared with `<run>-` -> counted;
/// - run ids are not prefix-free (`<run>-x-<topic>` of run `<run>-x` also
///   starts with `<run>-`), so another run's worktree in the same base can be
///   counted against `run` -> over-count, the restrictive side.
///
/// Not covered: a detached / un-namespaced worktree OUTSIDE the current
/// `worktree_base` (e.g. created under a different `CONDUKT_WORKTREE_BASE`)
/// carries neither witness and is not counted.
pub fn enforce_run_cap(
    repo: &Path,
    worktree_base: &Path,
    run: &str,
    max_parallel: usize,
) -> Result<()> {
    let live = worktree::list(repo).with_context(|| {
        format!(
            "max-parallel-undetermined: cannot list live worktrees of run '{run}'; \
             refusing to create (cap {max_parallel})"
        )
    })?;
    let base_canon = worktree::canonicalize_prefix(worktree_base);
    let path_prefix = format!("{run}-");
    let in_base = |p: &Path| -> bool {
        p.parent().is_some_and(|parent| {
            parent == worktree_base || worktree::canonicalize_prefix(parent) == base_canon
        })
    };
    let n = live
        .iter()
        .filter(|(p, b)| {
            let by_branch = b
                .as_deref()
                .and_then(|b| b.rsplit_once('/'))
                .is_some_and(|(prefix, _)| prefix == run || prefix.ends_with(&format!("/{run}")));
            let by_path = in_base(p)
                && match p.file_name().map(|n| n.to_str()) {
                    Some(Some(name)) => name.starts_with(&path_prefix),
                    // Non-UTF-8 name: membership cannot be determined -> count.
                    Some(None) => true,
                    None => false,
                };
            by_branch || by_path
        })
        .count();
    if n >= max_parallel {
        bail!(
            "max-parallel-exceeded: run '{run}' already has {n} live worktree(s) \
             (CONDUKT_MAX_PARALLEL / max_parallel = {max_parallel}); remove or merge one \
             before creating another"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use std::process::Command;

    fn git_repo(dir: &Path) {
        let out = harness_core::git_env::scrub(&mut Command::new("git"))
            .args(["init", "-q", "-b", "main"])
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(out.status.success(), "git init: {out:?}");
    }

    fn walk(dir: &Path) -> Vec<std::path::PathBuf> {
        let mut out = Vec::new();
        let Ok(rd) = std::fs::read_dir(dir) else {
            return out;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(walk(&p));
            }
            out.push(p);
        }
        out
    }

    /// The lock is published in condukt's state dir, keyed by run, and never
    /// inside the repo's working tree.
    #[test]
    fn run_cap_lock_lives_in_state_dir_not_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let repo = base.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git_repo(&repo);
        let mut cfg = Config::without_config_file();
        cfg.state_dir = base.join("state");
        let guard = acquire_run_cap_lock(&cfg, &repo, "run-x").unwrap();
        assert!(guard.held());
        let lock_name = format!("{RUN_CAP_LOCK_PREFIX}run-x.lock");
        let is_lock = |p: &std::path::PathBuf| {
            p.file_name().and_then(|n| n.to_str()) == Some(lock_name.as_str())
        };
        let in_state: Vec<_> = walk(&cfg.state_dir).into_iter().filter(is_lock).collect();
        assert_eq!(in_state.len(), 1, "lock under state dir: {in_state:?}");
        let in_repo: Vec<_> = walk(&repo).into_iter().filter(is_lock).collect();
        assert!(in_repo.is_empty(), "lock inside repo: {in_repo:?}");
    }

    /// Fail-closed: when the lock cannot be published (the state dir is a
    /// regular file, so the lock's directory cannot be created), acquisition is
    /// `Err` and the caller refuses the create; there is no unlocked proceed.
    #[test]
    fn run_cap_lock_failure_refuses() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let repo = base.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git_repo(&repo);
        let state_file = base.join("state-is-a-file");
        std::fs::write(&state_file, "x").unwrap();
        let mut cfg = Config::without_config_file();
        cfg.state_dir = state_file;
        let err = match acquire_run_cap_lock(&cfg, &repo, "run-x") {
            Ok(_) => panic!("lock acquired under an unusable state dir"),
            Err(e) => e.to_string(),
        };
        assert!(err.contains("max-parallel-undetermined"), "{err}");
        assert!(err.contains("refusing to create"), "{err}");
    }

    /// Fail-closed: a checkout whose `.git` gitfile is unparseable has an
    /// undetermined main worktree root, so the one lock file cannot be
    /// addressed and acquisition is `Err` (no per-checkout fallback lock).
    #[test]
    fn run_cap_lock_undetermined_root_refuses() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let checkout = base.join("broken");
        std::fs::create_dir_all(&checkout).unwrap();
        std::fs::write(checkout.join(".git"), "not a gitdir line\n").unwrap();
        let mut cfg = Config::without_config_file();
        cfg.state_dir = base.join("state");
        let err = match acquire_run_cap_lock(&cfg, &checkout, "run-x") {
            Ok(_) => panic!("lock acquired with an undetermined main worktree root"),
            Err(e) => e.to_string(),
        };
        assert!(err.contains("max-parallel-undetermined"), "{err}");
        assert!(
            walk(&cfg.state_dir).is_empty(),
            "no fallback lock published"
        );
    }
}
