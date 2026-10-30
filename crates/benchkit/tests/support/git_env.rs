//! Test-only copy of `harness_core::git_env` (benchkit does not depend on
//! harness-core). Source of truth: `crates/harness-core/src/git_env.rs`; keep
//! the variable list in step with it.
//!
//! A `cargo test` run inside a git hook inherits `GIT_DIR`, `GIT_INDEX_FILE`,
//! ... which override discovery, so a fixture `git` child with only
//! `current_dir` set acts on the OUTER repository. Pass such a `Command`
//! through [`scrub`].

use std::process::Command;

/// Variables that select the repository, its work tree, index, object store,
/// namespace, discovery boundaries or the config the child sees.
pub const INHERITED_GIT_ENV: [&str; 13] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_NAMESPACE",
    "GIT_PREFIX",
    "GIT_QUARANTINE_PATH",
    "GIT_CEILING_DIRECTORIES",
    "GIT_DISCOVERY_ACROSS_FILESYSTEM",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
];

/// Remove every [`INHERITED_GIT_ENV`] variable from `cmd`'s environment.
/// Returns `cmd` so it chains with the usual builder calls.
pub fn scrub(cmd: &mut Command) -> &mut Command {
    for k in INHERITED_GIT_ENV {
        cmd.env_remove(k);
    }
    cmd
}
