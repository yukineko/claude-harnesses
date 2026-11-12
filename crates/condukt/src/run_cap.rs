//! Per-run cap on LIVE worktrees (`Config::max_parallel`), enforced at
//! `condukt worktree create --run <id>`.

use anyhow::{bail, Context, Result};
use std::path::Path;

use crate::worktree;

/// Refuse when run `run` already has `max_parallel` or more LIVE worktrees.
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
