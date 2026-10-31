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

//!
//! [`pinned`] goes further for fixture git: scrubbing alone still lets git
//! DISCOVER a repo by walking up from `current_dir`. When a fixture directory
//! vanishes or is recreated without its `.git` (a concurrent run clobbering a
//! shared fixture, backlog 84a90410), that walk reaches the checkout the
//! target dir lives in, and a fixture `git commit -m init` commits ITS staged
//! index (RED repro: `scripts/tests/blastguard-wtfix-escape-staged-index.sh`).

use std::path::Path;
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

/// A scrubbed `git` command that can only ever operate on `dir` itself.
///
/// * `dir` must exist NOW: it is canonicalised, and a missing directory
///   panics (loudly, naming it) instead of running git anywhere else.
/// * `-C <dir>` plus `current_dir(<dir>)` start git there.
/// * `GIT_CEILING_DIRECTORIES=<parent of dir>` stops repository discovery at
///   `dir`: if `dir` has no `.git` (never had one, or was deleted/recreated
///   mid-run) git fails with "not a git repository" instead of walking up into
///   an enclosing checkout. A `dir` deleted after this call makes git fail on
///   its cwd; neither case can reach a repo outside `dir`.
///
/// Callers add their own `-c`/env configuration and arguments after this.
#[allow(dead_code)] // not every test that includes this module runs fixture git
pub fn pinned(dir: &Path) -> Command {
    let real = dir.canonicalize().unwrap_or_else(|e| {
        panic!(
            "fixture git dir {} is unusable ({e}); refusing to run git where it could \
             discover an enclosing repository",
            dir.display()
        )
    });
    let ceiling = real
        .parent()
        .unwrap_or_else(|| panic!("fixture git dir {} has no parent", real.display()))
        .to_path_buf();
    let mut cmd = Command::new("git");
    scrub(&mut cmd);
    cmd.current_dir(&real)
        .env("GIT_CEILING_DIRECTORIES", &ceiling)
        .arg("-C")
        .arg(&real);
    cmd
}
