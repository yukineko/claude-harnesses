//! Which trust-list membership a new worktree inherits at `worktree::create`.
//!
//! Backlog (2026-11-21, run run-20261121-114321-94421 t3): `condukt worktree
//! create` run with its cwd inside a LINKED worktree (`run4-integ`, made by a
//! plain `git worktree add` and so not itself listed) took that linked
//! worktree's toplevel as "the repo" (`main.rs` `run_worktree`:
//! `worktree::toplevel(cwd)`), found it unlisted, and silently registered
//! nothing, although the repository's main working tree was listed. t1/t2 of
//! the same run, created from the main tree, were registered. The documented
//! rule (`scripts/session-worktree-init.py`: "exactly when the main tree is
//! itself listed — the rule condukt's `worktree::create` applies") names the
//! MAIN tree, so the code did not do what both docs said.
//!
//! The source is listed when the repository's main working tree is listed, or
//! the cwd's own toplevel is (a listed linked worktree keeps propagating as
//! before). Membership only (`trust::is_listed`), never `is_trusted`: the
//! `HARNESS_TRUST_ALL` hatch must not mint a durable grant (backlog 5151605e).
//! This is still propagation, not escalation: a repo whose main tree is not
//! listed gives its worktrees nothing, from whichever worktree they are cut.
//!
//! This reads the repository's worktree list once, at creation, to name the
//! main tree; it never resolves an EXISTING worktree's trust from another one
//! (user ruling 2026-11-08: gates read only their own root's listing).

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// The repository's main working tree: the first `worktree <path>` record of
/// `git worktree list --porcelain` run in `repo`. `None` for a repository git
/// lists without one (a bare main entry has no checkout to be listed).
fn main_tree(repo: &Path) -> Result<Option<PathBuf>> {
    let listing = super::git(repo, &["worktree", "list", "--porcelain"])?;
    let mut records = listing.split("\n\n");
    let first = records.next().unwrap_or("");
    if first.lines().any(|l| l.trim() == "bare") {
        return Ok(None);
    }
    Ok(first
        .lines()
        .find_map(|l| l.strip_prefix("worktree "))
        .map(|p| {
            let p = PathBuf::from(p.trim());
            p.canonicalize().unwrap_or(p)
        }))
}

/// Should a worktree cut from `repo_canon` (the cwd's canonical toplevel) be
/// registered? `Err` when the main tree cannot be determined (git failed): the
/// caller refuses the create rather than return a worktree whose gate would
/// silently run nothing (CLAUDE.md §3: cannot-determine is not "unlisted").
pub(super) fn source_listed(repo_canon: &Path) -> Result<bool> {
    if harness_core::trust::is_listed(repo_canon) {
        return Ok(true);
    }
    let main = main_tree(repo_canon).with_context(|| {
        format!(
            "cannot determine the main working tree of {} to decide whether the new worktree \
             inherits its workspace trust; refusing rather than returning a worktree whose Stop \
             gate (donegate) might silently run no checks",
            repo_canon.display()
        )
    })?;
    Ok(main.is_some_and(|m| m != repo_canon && harness_core::trust::is_listed(&m)))
}
