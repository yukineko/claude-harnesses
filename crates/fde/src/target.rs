//! Target resolution and the fde state directory.
//!
//! [`resolve`] and [`state_dir`] are implemented; both answer `Undetermined`
//! (never a guessed value) when they cannot reach a conclusion.

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
