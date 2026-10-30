//! Strip inherited `GIT_*` variables that relocate the repository a `git`
//! child process operates on.
//!
//! Hooks are run by processes that themselves run inside git: a pre-commit /
//! pre-push hook exports `GIT_DIR`, `GIT_INDEX_FILE`, … to everything it
//! launches, and a user can have `GIT_DIR` set in the shell Claude Code was
//! started from. A `git` child inherits those, and they **override discovery**:
//! with `GIT_DIR=<repo B>/.git` set, `git -C <path in repo A>` (or
//! `current_dir`) answers about repo B, and `git -C <plain dir>` answers about
//! repo B instead of saying "not a repository". `-C` alone does not help.
//!
//! Any probe whose question is "which repository, if any, contains this path
//! on disk" must therefore pass its `Command` through [`scrub`], so the repo is
//! found from the working directory alone. Measured: blastguard's
//! recoverability probe judged a truncation of a non-repo file as a clean
//! tracked file of the unrelated repo named by an inherited `GIT_DIR`, and
//! allowed it (`crates/blastguard/tests/reversible_probe_inherited_git_dir.rs`).
//!
//! This is NOT for code that deliberately honours the caller's `GIT_DIR`
//! (e.g. [`crate::git_probe::probe_repo`], used by gates that run inside a
//! hook and are meant to act on the repository the hook was invoked for).
//! Test fixtures that build throwaway repos under a hook-driven `cargo test`
//! are the other intended user.

use std::process::Command;

/// Variables that select the repository, its work tree, index, object store,
/// namespace, discovery boundaries or the config the child sees.
///
/// * `GIT_DIR` / `GIT_WORK_TREE` / `GIT_COMMON_DIR` — which repository and
///   work tree; each overrides discovery from the working directory.
/// * `GIT_INDEX_FILE` / `GIT_OBJECT_DIRECTORY` /
///   `GIT_ALTERNATE_OBJECT_DIRECTORIES` / `GIT_QUARANTINE_PATH` — which index
///   and object store (the quarantine path is set during `pre-receive`).
/// * `GIT_NAMESPACE` — which refs are visible.
/// * `GIT_PREFIX` — set by git for aliases/hooks; makes relative pathspecs
///   resolve against a different subdirectory.
/// * `GIT_CEILING_DIRECTORIES` / `GIT_DISCOVERY_ACROSS_FILESYSTEM` — where
///   upward discovery stops. An inherited ceiling can hide the repository that
///   really contains the path and make it read as "not a repository".
/// * `GIT_CONFIG_PARAMETERS` / `GIT_CONFIG_COUNT` — `-c` overrides inherited
///   from a parent git; removing `GIT_CONFIG_COUNT` alone disables every
///   `GIT_CONFIG_KEY_n` / `GIT_CONFIG_VALUE_n` pair.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrub_removes_every_listed_variable() {
        let mut cmd = Command::new("git");
        for k in INHERITED_GIT_ENV {
            cmd.env(k, "/inherited");
        }
        scrub(&mut cmd);
        let envs: Vec<_> = cmd.get_envs().collect();
        for k in INHERITED_GIT_ENV {
            let entry = envs
                .iter()
                .find(|(name, _)| *name == std::ffi::OsStr::new(k))
                .unwrap_or_else(|| panic!("{k} not recorded as removed"));
            assert_eq!(entry.1, None, "{k} must be removed, not set");
        }
    }
}
