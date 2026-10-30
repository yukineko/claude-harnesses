//! Strip inherited git variables that relocate the repository a `git` child
//! (or the blastguard binary's own git probes) operates on.
//!
//! A git hook exports e.g. `GIT_DIR` / `GIT_INDEX_FILE` to the processes it
//! runs, so a `cargo test` launched from a pre-commit/pre-push hook would make
//! fixture `git init/add/commit/worktree add` mutate the OUTER repo instead of
//! the fixture directory (RED repro:
//! `cd_and_chain_equivalence::fixture_git_ignores_inherited_git_dir`).
//! Every fixture git helper — and every test spawn of the blastguard binary,
//! whose `reversible.rs` probes run `git` — passes its `Command` through
//! [`scrub`] so the repo is found from `current_dir` / `-C` alone.

use std::process::Command;

/// Variables that select the repo, its index/object store or the config the
/// child sees. `GIT_CONFIG_COUNT` alone disables `GIT_CONFIG_KEY_n` entries.
pub const INHERITED_GIT_ENV: [&str; 12] = [
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
