//! Quarantine for a worktree directory that a failed forced removal left behind.
//!
//! A forced `git worktree remove --force` that fails PART-WAY (e.g. an entry
//! under an ignored `target/` cannot be unlinked) can still delete the
//! worktree's admin dir, leaving a directory that is neither removed nor
//! registered. That orphan is quarantine-moved to `<base>.quarantine/<name>-<secs>`
//! (rename only; nothing is deleted and nothing is re-registered), where `<base>`
//! is the directory holding the worktree. The quarantine sits beside the worktree
//! base, not inside it, so `unregistered_dirs` does not report it as a new
//! orphan, and the rename is atomic on the same filesystem.
//!
//! Fail-closed: if the registrations cannot be read, or the directory cannot be
//! created or renamed, the leftover stays in place and the returned error says
//! so. The caller always gets `Err`, never success.

use anyhow::{anyhow, bail, Context, Result};
use std::path::{Path, PathBuf};

use super::list_all;

/// Turn git's refusal of a forced removal into the final error for `remove`,
/// quarantine-moving the leftover first when it is an unregistered orphan.
/// Always an error: a failed forced removal is never reported as success.
pub(super) fn fail_forced_removal(repo: &Path, path: &Path, cause: anyhow::Error) -> anyhow::Error {
    match quarantine_unregistered_leftover(repo, path) {
        Ok(Some(dest)) => cause.context(format!(
            "could not force-remove worktree {}; the partially removed leftover was moved to {} (nothing deleted)",
            path.display(),
            dest.display()
        )),
        Ok(None) => cause.context(format!("could not force-remove worktree {}", path.display())),
        Err(q) => cause.context(format!(
            "could not force-remove worktree {}; {q:#}",
            path.display()
        )),
    }
}

/// Move a leftover directory that is no longer a registered worktree into the
/// quarantine. `Ok(None)` when there is nothing to move (the path is gone, or
/// git still lists it as a registered worktree, which is then left alone).
/// `Ok(Some(dest))` after a successful rename.
fn quarantine_unregistered_leftover(repo: &Path, path: &Path) -> Result<Option<PathBuf>> {
    if !path.exists() {
        return Ok(None);
    }
    let canon = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let registered = list_all(repo)
        .with_context(|| {
            format!(
                "cannot read worktree registrations, so {} is left in place",
                path.display()
            )
        })?
        .into_iter()
        .any(|(p, _, _)| p.canonicalize().unwrap_or(p) == canon);
    if registered {
        return Ok(None);
    }
    let name = path
        .file_name()
        .ok_or_else(|| anyhow!("cannot quarantine {}: no file name", path.display()))?;
    let base = path
        .parent()
        .ok_or_else(|| anyhow!("cannot quarantine {}: no parent directory", path.display()))?;
    let base_name = base
        .file_name()
        .ok_or_else(|| anyhow!("cannot quarantine {}: parent has no name", path.display()))?;
    let root = base.with_file_name(format!("{}.quarantine", base_name.to_string_lossy()));
    std::fs::create_dir_all(&root).with_context(|| {
        format!(
            "cannot create quarantine dir {}; {} is left in place",
            root.display(),
            path.display()
        )
    })?;
    let dest = root.join(format!(
        "{}-{}",
        name.to_string_lossy(),
        crate::state::now_secs()
    ));
    if dest.exists() {
        bail!(
            "cannot quarantine {}: {} already exists; {} is left in place",
            path.display(),
            dest.display(),
            path.display()
        );
    }
    std::fs::rename(path, &dest).with_context(|| {
        format!(
            "cannot move {} to {}; it is left in place",
            path.display(),
            dest.display()
        )
    })?;
    Ok(Some(dest))
}
