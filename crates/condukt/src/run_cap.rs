//! Per-run cap on LIVE worktrees (`Config::max_parallel`), enforced at
//! `condukt worktree create --run <id>`.

use anyhow::{bail, Context, Result};
use std::path::Path;

use crate::worktree;

/// Refuse when run `run` already has `max_parallel` or more LIVE worktrees
/// (registered, non-primary, branch namespaced under `run`). If the live set
/// cannot be determined (git listing fails) -> refuse (fail-closed).
pub fn enforce_run_cap(repo: &Path, run: &str, max_parallel: usize) -> Result<()> {
    let live = worktree::list(repo).with_context(|| {
        format!(
            "max-parallel-undetermined: cannot list live worktrees of run '{run}'; \
             refusing to create (cap {max_parallel})"
        )
    })?;
    let n = live
        .iter()
        .filter(|(_, b)| {
            b.as_deref()
                .and_then(|b| b.rsplit_once('/'))
                .is_some_and(|(prefix, _)| prefix == run || prefix.ends_with(&format!("/{run}")))
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
