//! Target resolution and the fde state directory.
//!
//! [`resolve`] and [`state_dir`] are implemented; both answer `Undetermined`
//! (never a guessed value) when they cannot reach a conclusion. [`presence`]
//! tells whether a registered root still exists ("gone" only on `NotFound`),
//! and [`forget_candidates`] computes what `fde forget` removes.

use std::path::{Path, PathBuf};

use harness_core::verdict::Determination;
use sha2::{Digest, Sha256};

use crate::git;

/// Environment variable that overrides the fde state directory.
pub const STATE_DIR_ENV: &str = "FDE_STATE_DIR";

/// A resolved target: the canonical git top-level and the key under which its
/// state is stored in the state directory.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ResolvedTarget {
    /// Canonical (symlink-resolved, absolute) git top-level of the target.
    pub root: PathBuf,
    /// Stable key derived from `root` alone (see [`state_key`]); two spellings
    /// of the same repo map to the same key.
    pub state_key: String,
}

/// The fde state directory: the `FDE_STATE_DIR` env var when set and
/// non-empty, else `.fde` under the home directory
/// (`harness_core::config::home`). `Undetermined` when the env var is unset or
/// empty and the home directory is unresolved (the
/// `harness_core::config::UNDETERMINED_HOME` sentinel). A relative
/// `FDE_STATE_DIR` is also `Undetermined`: it would silently name a different
/// directory depending on the invocation cwd.
#[track_caller]
pub fn state_dir() -> Determination<PathBuf> {
    if let Some(v) = std::env::var_os(STATE_DIR_ENV) {
        if !v.is_empty() {
            let p = PathBuf::from(v);
            if !p.is_absolute() {
                return Determination::undetermined(format!(
                    "{STATE_DIR_ENV}={} is not an absolute path",
                    p.display()
                ));
            }
            return Determination::known(p);
        }
    }
    let home = harness_core::config::home();
    if home == Path::new(harness_core::config::UNDETERMINED_HOME) || !home.is_absolute() {
        return Determination::undetermined(format!(
            "{STATE_DIR_ENV} is unset and the home directory cannot be resolved"
        ));
    }
    Determination::known(home.join(".fde"))
}

/// The state key of a canonical git top-level: the lowercase hex SHA-256 of
/// the path's raw OS bytes (64 hex chars). Depends on `root` only, so it is
/// stable across invocations, and distinct roots get distinct keys.
pub fn state_key(root: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let digest = Sha256::digest(root.as_os_str().as_bytes());
    let mut hex = String::with_capacity(digest.len() * 2);
    for b in digest {
        hex.push_str(&format!("{b:02x}"));
    }
    hex
}

/// Resolve `target` (or, when `None`, the invocation `cwd`) to its canonical
/// git top-level.
///
/// 1. The starting path is `target` when absolute, `cwd.join(target)` when
///    relative, and `cwd` when `target` is `None`.
/// 2. It is canonicalized (symlinks resolved). A path that does not exist is
///    `Undetermined` — nothing is walked up to an existing ancestor.
/// 3. `git rev-parse --show-toplevel` runs in that directory (in the parent
///    directory when the path is a file) with inherited `GIT_*` scrubbed; see
///    [`crate::git::show_toplevel`]. A failed or empty git answer (including
///    "not a git work tree") is `Undetermined`.
/// 4. git's top-level is canonicalized again and must contain the starting
///    path; otherwise `Undetermined`.
///
/// There is no fallback to `cwd` or to the starting path at any step. The
/// whole git top-level is the target; sub-project markers (`Cargo.toml`,
/// `package.json`, …) are not consulted.
#[track_caller]
pub fn resolve(target: Option<&Path>, cwd: &Path) -> Determination<ResolvedTarget> {
    let start = match target {
        None => cwd.to_path_buf(),
        Some(t) if t.is_absolute() => t.to_path_buf(),
        Some(t) => cwd.join(t),
    };
    let canon = match std::fs::canonicalize(&start) {
        Ok(c) => c,
        Err(e) => {
            return Determination::undetermined(format!(
                "target {} cannot be canonicalized (missing or unreadable): {e}",
                start.display()
            ))
        }
    };
    let probe_dir = if canon.is_dir() {
        canon.clone()
    } else {
        match canon.parent() {
            Some(p) => p.to_path_buf(),
            None => {
                return Determination::undetermined(format!(
                    "target {} is not a directory and has no parent",
                    canon.display()
                ))
            }
        }
    };
    let top = match git::show_toplevel(&probe_dir) {
        Determination::Known(t) => t,
        Determination::Undetermined(why) => return Determination::Undetermined(why),
    };
    let root = match std::fs::canonicalize(&top) {
        Ok(r) => r,
        Err(e) => {
            return Determination::undetermined(format!(
                "git top-level {} cannot be canonicalized: {e}",
                top.display()
            ))
        }
    };
    if !canon.starts_with(&root) {
        return Determination::undetermined(format!(
            "git reported top-level {} which does not contain the target {}",
            root.display(),
            canon.display()
        ));
    }
    let state_key = state_key(&root);
    Determination::known(ResolvedTarget { root, state_key })
}

/// Whether a registered root is still there, as observed by
/// `std::fs::symlink_metadata` (the root itself is not followed, so a root
/// that is now a dangling symlink is present, not gone).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presence {
    /// Something exists at the root path.
    Present,
    /// The root path does not exist (`NotFound`): a determined fact.
    Gone,
}

/// Observe whether `root` exists. `Known(Gone)` only for `NotFound`; every
/// other stat error (permission denied, IO, …) is `Undetermined` — the root
/// may still be there.
#[track_caller]
pub fn presence(root: &Path) -> Determination<Presence> {
    match std::fs::symlink_metadata(root) {
        Ok(_) => Determination::known(Presence::Present),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Determination::known(Presence::Gone),
        Err(e) => Determination::undetermined(format!(
            "cannot tell whether registered target {} still exists: {e}",
            root.display()
        )),
    }
}

/// The roots a path that does not exist may have been registered under: its
/// [`crate::gate::canonical_lenient`] form (nearest existing ancestor
/// canonicalized, so a symlinked-parent spelling matches the canonical root
/// fde recorded), then the path itself with `.` / `..` applied lexically when
/// that differs (the exact recorded root, should the ancestors have changed).
/// `start` must be absolute; `Undetermined` when it is not or when
/// canonicalization fails with anything but `NotFound`.
#[track_caller]
pub fn absent_root_candidates(start: &Path) -> Determination<Vec<PathBuf>> {
    let canon = match crate::gate::canonical_lenient(start) {
        Determination::Known(c) => c,
        Determination::Undetermined(why) => return Determination::Undetermined(why),
    };
    let mut lexical = PathBuf::new();
    for c in start.components() {
        match c {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                lexical.pop();
            }
            other => lexical.push(other),
        }
    }
    let mut out = vec![canon];
    if !out.contains(&lexical) {
        out.push(lexical);
    }
    Determination::known(out)
}

/// What `fde forget [TARGET]` acts on: the `(root, state_key)` candidates, in
/// order, and — when the key came from the canonical-path fallback below — the
/// git failure that forced it, so a refusal can say why git's answer was not
/// used.
#[derive(Debug, Clone)]
pub struct ForgetPlan {
    /// `(root, state_key)` pairs; never empty.
    pub candidates: Vec<(PathBuf, String)>,
    /// Why git resolution of an existing TARGET failed, when it did.
    pub git_failure: Option<String>,
}

/// The [`ForgetPlan`] for `fde forget [TARGET]` (see `main.rs`); the caller
/// removes the state of the first candidate whose state directory exists.
///
/// * no `target`: [`resolve`] of the cwd (its git top-level);
/// * an existing `target` (absolute, or relative to `cwd`): [`resolve`] of it
///   (its git top-level), so a subdirectory names its repository; when that
///   git resolution fails (e.g. a registered root whose `.git` was removed is
///   no longer a work tree), the canonical path of `target` itself — a
///   registered root that stopped being a repository is still forgettable,
///   while an unregistered directory has no state there and is refused. A
///   failure to canonicalize `target` is `Undetermined` (naming the git
///   failure too);
/// * a `target` that does not exist (`NotFound`): every
///   [`absent_root_candidates`] entry, in order, so a gone target is reachable
///   through a relative or symlinked spelling of its former path;
/// * any other stat error, or a resolution failure of the cwd: `Undetermined`.
#[track_caller]
pub fn forget_candidates(target: Option<&Path>, cwd: &Path) -> Determination<ForgetPlan> {
    let plan = |candidates, git_failure| ForgetPlan {
        candidates,
        git_failure,
    };
    let Some(t) = target else {
        return match resolve(None, cwd) {
            Determination::Known(r) => {
                Determination::known(plan(vec![(r.root, r.state_key)], None))
            }
            Determination::Undetermined(why) => Determination::Undetermined(why),
        };
    };
    let start = if t.is_absolute() {
        t.to_path_buf()
    } else {
        cwd.join(t)
    };
    match std::fs::symlink_metadata(&start) {
        Ok(_) => match resolve(Some(&start), cwd) {
            Determination::Known(r) => {
                Determination::known(plan(vec![(r.root, r.state_key)], None))
            }
            Determination::Undetermined(git_why) => match std::fs::canonicalize(&start) {
                Ok(canon) => {
                    let k = state_key(&canon);
                    Determination::known(plan(vec![(canon, k)], Some(git_why.to_string())))
                }
                Err(e) => Determination::undetermined(format!(
                    "{git_why}; and {} cannot be canonicalized: {e}",
                    start.display()
                )),
            },
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            match absent_root_candidates(&start) {
                Determination::Known(c) => Determination::known(plan(
                    c.into_iter()
                        .map(|p| {
                            let k = state_key(&p);
                            (p, k)
                        })
                        .collect(),
                    None,
                )),
                Determination::Undetermined(why) => Determination::Undetermined(why),
            }
        }
        Err(e) => Determination::undetermined(format!(
            "cannot tell whether {} exists: {e}",
            start.display()
        )),
    }
}
