//! Target resolution and the fde state directory.
//!
//! SCAFFOLD: every function here answers `Undetermined`. The contract the
//! implementation must meet is documented on each function and in `main.rs`.

use std::path::{Path, PathBuf};

use harness_core::verdict::Determination;

/// Environment variable that overrides the fde state directory.
#[expect(
    dead_code,
    reason = "scaffold: read by state_dir once implemented; this expect then fails and must be removed"
)]
pub const STATE_DIR_ENV: &str = "FDE_STATE_DIR";

/// A resolved target: the canonical git top-level and the key under which its
/// state is stored in the state directory.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ResolvedTarget {
    /// Canonical (symlink-resolved, absolute) git top-level of the target.
    pub root: PathBuf,
    /// Stable key derived from `root`; two spellings of the same repo map to
    /// the same key.
    pub state_key: String,
}

/// The fde state directory: the `FDE_STATE_DIR` env var when set and
/// non-empty, else `.fde` under the home directory. Must be `Undetermined`
/// when neither can be resolved.
///
/// SCAFFOLD: always `Undetermined`.
#[track_caller]
pub fn state_dir() -> Determination<PathBuf> {
    Determination::undetermined("fde: state_dir is not implemented (scaffold)")
}

/// Resolve `target` (or, when `None`, the invocation `cwd`) to its canonical
/// git top-level. A relative `target` resolves against `cwd`. Not a git work
/// tree, a missing path, or a failed git probe must be `Undetermined`.
///
/// SCAFFOLD: always `Undetermined`.
#[track_caller]
pub fn resolve(target: Option<&Path>, cwd: &Path) -> Determination<ResolvedTarget> {
    let _ = (target, cwd);
    Determination::undetermined("fde: target resolution is not implemented (scaffold)")
}
