//! Test-only copy of `harness_core::git_env` (`crates/harness-core/src/git_env.rs`).
//!
//! deepwiki does not depend on harness-core, so the 13-variable list and
//! `scrub` are duplicated here. Keep the list in sync with
//! `INHERITED_GIT_ENV` in harness-core.
//!
//! When `cargo test` runs inside a git hook, `GIT_DIR` / `GIT_INDEX_FILE` / ...
//! are inherited and override repository discovery, so a fixture `git` call
//! (or a spawned plugin binary's git probe) would act on the OUTER repo.

use std::process::Command;

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
pub fn scrub(cmd: &mut Command) -> &mut Command {
    for k in INHERITED_GIT_ENV {
        cmd.env_remove(k);
    }
    cmd
}
