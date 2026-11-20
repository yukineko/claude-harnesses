//! Quarantine for a worktree directory that a removal left behind part-way.
//!
//! Two shapes are quarantine-moved, both a git worktree removal that stopped
//! part-way:
//!
//! 1. A forced `git worktree remove --force` that fails PART-WAY (e.g. an entry
//!    under an ignored `target/` cannot be unlinked) can still delete the
//!    worktree's admin dir, leaving a directory that is neither removed nor
//!    registered ([`force_remove`] / [`fail_forced_removal`], backlog
//!    c96e756a; always reported as an error).
//! 2. A directory whose `.git` file is already gone while tracked and ignored
//!    residue remains (backlog 4503c90d, observed on WSL drvfs): git declines the
//!    plain removal, the content cannot be preserved and its dirtiness cannot be
//!    read ([`resolve_undetermined`]).
//!
//! # Positive evidence (both shapes)
//!
//! "No `.git`, not listed by `git worktree list`" is true of ANY plain
//! directory, so absence of evidence never authorizes a move. A directory is
//! moved only when ALL of these are observed ([`location_evidence`] plus a
//! registration check):
//!
//! - `path` itself is not a symlink (the rename would move the link, not the
//!   directory it names) and canonicalizes;
//! - its canonical path is STRICTLY inside the canonical configured
//!   `worktree_base` (where condukt creates task worktrees; `Config::load`);
//! - it is neither a primary working tree of `repo` nor inside one (so a tracked
//!   subdirectory of the primary repo is never moved);
//! - git's own admin data for `repo` names exactly this canonical path as a
//!   worktree: for shape 1, `git worktree list --porcelain` listed it BEFORE the
//!   forced removal ran (git's admin dir is what the failed removal deletes, so
//!   the evidence is captured first); for shape 2, it is listed NOW and every
//!   entry naming it is `prunable`, and `<path>/.git` does not exist.
//!
//! The registration check is what separates a former worktree from a plain
//! directory: git only lists a path it created an admin dir
//! (`<repo>/.git/worktrees/<id>/gitdir`) for. The base check confines moves to
//! condukt's worktree area. Neither alone suffices; the run-state record is NOT
//! used, because a task can be pointed at any directory with `state set`.
//!
//! Anything not observed, or not observable (config, canonicalization, the
//! listing or the `.git` inspection fails), refuses: the directory stays in place
//! and the error says why.
//!
//! The orphan is moved to `<parent>.quarantine/<name>-<secs>` (rename only;
//! nothing is deleted and nothing is re-registered), where `<parent>` is the
//! directory holding the worktree. The quarantine sits beside that directory,
//! not inside it, so `unregistered_dirs` does not report it as a new orphan. A
//! rename that fails (e.g. across filesystems) is NOT retried as copy+delete.

use anyhow::{anyhow, bail, Context, Result};
use std::path::{Path, PathBuf};

use crate::config::Config;

/// `git worktree remove --force <path>`, quarantining a part-way leftover on
/// failure. The positive evidence that `path` is a registered worktree under
/// `worktree_base` is taken BEFORE the force (see the module docs, shape 1).
pub(super) fn force_remove(repo: &Path, path: &Path) -> Result<()> {
    let evidence = pre_force_evidence(repo, path);
    let path_str = path.to_string_lossy().to_string();
    match super::git(repo, &["worktree", "remove", "--force", &path_str]) {
        Ok(_) => Ok(()),
        Err(e) => Err(fail_forced_removal(repo, path, evidence, e)),
    }
}

/// Canonical path of `path` when it is located like a condukt task worktree
/// AND `repo`'s worktree listing names it (any registration state). `Err` is
/// the reason no evidence exists (the caller then never moves the leftover).
fn pre_force_evidence(repo: &Path, path: &Path) -> Result<PathBuf, String> {
    let listing = super::git(repo, &["worktree", "list", "--porcelain"])
        .map_err(|e| format!("cannot read worktree registrations ({e:#})"))?;
    let canon = location_evidence(repo, path, &listing)?;
    match registration_state(&listing, &canon) {
        Registration::Absent => Err(format!(
            "{} is not a registered git worktree of {}",
            canon.display(),
            repo.display()
        )),
        Registration::Prunable | Registration::Live => Ok(canon),
    }
}

/// Turn git's refusal of a forced removal into the final error for `remove`,
/// quarantine-moving the leftover first when `evidence` (captured before the
/// force) showed it was a registered worktree under `worktree_base` and it is
/// now no longer registered. Always an error: a failed forced removal is never
/// reported as success.
fn fail_forced_removal(
    repo: &Path,
    path: &Path,
    evidence: Result<PathBuf, String>,
    cause: anyhow::Error,
) -> anyhow::Error {
    let outcome = match evidence {
        Ok(canon) => quarantine_unregistered_leftover(repo, path, &canon),
        Err(no) => {
            return cause.context(format!(
                "could not force-remove worktree {}; it is not quarantined because there is \
                 no positive evidence it is a condukt task worktree ({no}). The directory is \
                 left in place",
                path.display()
            ))
        }
    };
    match outcome {
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

/// Move a leftover that WAS a registered worktree (`canon`, observed before the
/// force) and is no longer registered. `Ok(None)` when there is nothing to move
/// (the path is gone, or git still lists it, which is then left alone).
fn quarantine_unregistered_leftover(
    repo: &Path,
    path: &Path,
    canon: &Path,
) -> Result<Option<PathBuf>> {
    if !path.exists() {
        return Ok(None);
    }
    let listing = super::git(repo, &["worktree", "list", "--porcelain"]).with_context(|| {
        format!(
            "cannot read worktree registrations, so {} is left in place",
            path.display()
        )
    })?;
    if registration_state(&listing, canon) != Registration::Absent {
        return Ok(None);
    }
    move_to_quarantine(path).map(Some)
}

/// The `Undetermined`-dirtiness arm of `remove_worktree_preserving`: git
/// declined the plain removal, the content could not be preserved (`why`) and
/// its dirtiness could not be read (`undecided`).
///
/// `Ok(())` ONLY when the positive evidence of shape 2 (module docs) is
/// observed: the directory was renamed into the quarantine (nothing deleted),
/// the dangling registration was dropped with `git worktree prune` (so the
/// branch is no longer "checked out"), and the destination is printed to
/// stderr. Otherwise `Err` — the refusal as before, with the directory left in
/// place; a failed prune after the move is also `Err`, naming where it went.
pub(super) fn resolve_undetermined(
    repo: &Path,
    path: &Path,
    why: &str,
    undecided: &dyn std::fmt::Display,
) -> Result<()> {
    let refusal = format!(
        "refusing to remove worktree {}: git declined to remove it, its content could not be \
         preserved ({why}), and whether it holds uncommitted work could not be determined \
         ({undecided}).",
        path.display()
    );
    let dest = match part_way_removed_evidence(repo, path) {
        Ok(()) => match move_to_quarantine(path) {
            Ok(dest) => dest,
            Err(q) => bail!("{refusal} {q:#}"),
        },
        Err(no) => bail!(
            "{refusal} It is not quarantined because there is no positive evidence it is a \
             part-way-removed condukt task worktree ({no}). The directory is left in place."
        ),
    };
    super::git(repo, &["worktree", "prune"]).with_context(|| {
        format!(
            "part-way-removed worktree {} was moved to {} (nothing deleted), but \
             `git worktree prune` failed afterwards",
            path.display(),
            dest.display()
        )
    })?;
    eprintln!(
        "condukt: worktree {} was a part-way removal (no .git; git lists it only as prunable); \
         its leftover was moved to {} (nothing deleted)",
        path.display(),
        dest.display()
    );
    Ok(())
}

/// Shape-2 evidence: `<path>/.git` does not exist, the location checks hold,
/// and `repo`'s listing names the canonical path with every entry `prunable`.
/// `Err` carries why the evidence is missing or could not be observed.
fn part_way_removed_evidence(repo: &Path, path: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(path.join(".git")) {
        Ok(_) => return Err(format!("{}/.git exists", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("cannot inspect {}/.git ({e})", path.display())),
    }
    let listing = super::git(repo, &["worktree", "list", "--porcelain"])
        .map_err(|e| format!("cannot read worktree registrations ({e:#})"))?;
    let canon = location_evidence(repo, path, &listing)?;
    match registration_state(&listing, &canon) {
        Registration::Prunable => Ok(()),
        Registration::Absent => Err(format!(
            "git has no worktree registration for {}",
            canon.display()
        )),
        Registration::Live => Err(format!(
            "git lists {} as a live (non-prunable) worktree",
            canon.display()
        )),
    }
}

/// The location half of the evidence: `path` is not a symlink, and its
/// canonical path is strictly inside the canonical configured `worktree_base`
/// and not inside any primary working tree of `repo` (`listing` is `repo`'s
/// `git worktree list --porcelain`). Returns the canonical path.
fn location_evidence(repo: &Path, path: &Path, listing: &str) -> Result<PathBuf, String> {
    let meta = std::fs::symlink_metadata(path)
        .map_err(|e| format!("cannot inspect {} ({e})", path.display()))?;
    if meta.file_type().is_symlink() {
        return Err(format!("{} is a symlink", path.display()));
    }
    let canon = path
        .canonicalize()
        .map_err(|e| format!("cannot canonicalize {} ({e})", path.display()))?;
    let cfg = Config::load().map_err(|e| format!("cannot load condukt's configuration ({e})"))?;
    let base = cfg.worktree_base.canonicalize().map_err(|e| {
        format!(
            "cannot canonicalize worktree_base {} ({e})",
            cfg.worktree_base.display()
        )
    })?;
    if canon == base || !canon.starts_with(&base) {
        return Err(format!(
            "{} is not strictly inside worktree_base {}",
            canon.display(),
            base.display()
        ));
    }
    let primaries = super::primary_witnesses(repo, listing);
    if primaries.is_empty() {
        return Err(format!(
            "cannot determine the primary working tree of {}",
            repo.display()
        ));
    }
    if let Some(p) = primaries.iter().find(|p| canon.starts_with(p)) {
        return Err(format!(
            "{} is inside the primary working tree {}",
            canon.display(),
            p.display()
        ));
    }
    Ok(canon)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Registration {
    /// No entry names the path.
    Absent,
    /// Every entry naming the path carries a `prunable` line.
    Prunable,
    /// At least one entry names the path without `prunable` (restrictive side).
    Live,
}

/// How `git worktree list --porcelain` output registers `canon`. An entry path
/// that cannot be canonicalized is compared as written.
fn registration_state(listing: &str, canon: &Path) -> Registration {
    let mut state = Registration::Absent;
    // (entry names `canon`, entry has a `prunable` line)
    let mut cur: Option<(bool, bool)> = None;
    let finish = |cur: Option<(bool, bool)>, state: &mut Registration| {
        if let Some((true, prunable)) = cur {
            *state = if *state == Registration::Live || !prunable {
                Registration::Live
            } else {
                Registration::Prunable
            };
        }
    };
    for line in listing.lines().chain(std::iter::once("")) {
        if let Some(p) = line.strip_prefix("worktree ") {
            finish(cur.take(), &mut state);
            let p = PathBuf::from(p);
            let matches = p.canonicalize().unwrap_or(p) == canon;
            cur = Some((matches, false));
        } else if line == "prunable" || line.starts_with("prunable ") {
            if let Some(c) = cur.as_mut() {
                c.1 = true;
            }
        } else if line.is_empty() {
            finish(cur.take(), &mut state);
        }
    }
    state
}

/// Rename `path` to `<parent>.quarantine/<name>-<secs>`. Rename only: a failure
/// leaves `path` in place and is returned as `Err` (never copy+delete).
fn move_to_quarantine(path: &Path) -> Result<PathBuf> {
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
    Ok(dest)
}
