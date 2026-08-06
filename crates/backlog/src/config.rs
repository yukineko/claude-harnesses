use std::path::PathBuf;

use harness_core::config::{base_dir, expand_tilde};
use serde::Deserialize;

pub struct Config {
    pub enabled: bool,
    pub store_dir: PathBuf,
    pub inject_limit: usize,
    /// True when `config.toml` named `store_dir` explicitly. An operator who
    /// pinned a location keeps it: per-project resolution is a DEFAULT, not an
    /// override, so `tasks_path_for` must not silently relocate their store.
    pub store_dir_pinned: bool,
}

#[derive(Default, Deserialize)]
struct FileConfig {
    enabled: Option<bool>,
    store_dir: Option<String>,
    inject_limit: Option<usize>,
}

impl Config {
    pub fn load() -> Self {
        let base = base_dir("backlog");
        let mut cfg = Config {
            enabled: true,
            store_dir: base.clone(),
            inject_limit: 4000,
            store_dir_pinned: false,
        };
        if let Ok(txt) = std::fs::read_to_string(base.join("config.toml")) {
            if let Ok(fc) = toml::from_str::<FileConfig>(&txt) {
                if let Some(v) = fc.enabled {
                    cfg.enabled = v;
                }
                if let Some(v) = fc.store_dir {
                    cfg.store_dir = expand_tilde(&v);
                    cfg.store_dir_pinned = true;
                }
                if let Some(v) = fc.inject_limit {
                    cfg.inject_limit = v;
                }
            }
        }
        cfg
    }

    /// The store for one project: `<repo root>/.backlog/tasks.toml`.
    ///
    /// backlog is cross-project, so a single global file merged every
    /// project's queue into one path that no repo could review, commit, or
    /// ship with the work it describes. Resolving the store from the task's
    /// OWN project keeps each queue inside the tree it talks about, where it
    /// is an ordinary tracked file.
    ///
    /// Resolution order, and what each step means:
    ///   1. `store_dir` pinned in `config.toml` — an operator who named a
    ///      location keeps it. Per-project resolution is the DEFAULT, not an
    ///      override.
    ///   2. the repo root containing `project` (or the cwd when the
    ///      subcommand carries no project) — `<root>/.backlog`.
    ///   3. no repo root found — the legacy `~/.backlog`.
    ///
    /// Step 3 is deliberately not an error. A path with no repo root cannot be
    /// resolved into a project store, and inventing one would scatter tasks
    /// into whatever directory the caller happened to stand in; the legacy
    /// path at least leaves them where every existing reader already looks.
    pub fn tasks_path_for(&self, project: Option<&str>) -> PathBuf {
        self.store_dir_for(project).join("tasks.toml")
    }

    pub fn store_dir_for(&self, project: Option<&str>) -> PathBuf {
        if self.store_dir_pinned {
            return self.store_dir.clone();
        }
        let start = project
            .map(PathBuf::from)
            .or_else(|| std::env::current_dir().ok());
        match start.as_deref().and_then(repo_root) {
            Some(root) => root.join(".backlog"),
            None => self.store_dir.clone(),
        }
    }

    pub fn disabled_env() -> bool {
        std::env::var("BACKLOG_DISABLE")
            .map(|v| v == "1")
            .unwrap_or(false)
    }
}

/// Nearest ancestor of `start` (inclusive) that holds a `.git` entry.
///
/// `.git` is a DIRECTORY in an ordinary clone and a FILE in a git worktree, so
/// `exists()` is the right test rather than `is_dir()`: a worktree gets its own
/// `.backlog`, which then merges into main like any other tracked file (this
/// repo's CLAUDE.md §8 requires work to happen in worktrees, so a worktree
/// resolving to the main tree's store would push edits into a tree the session
/// is not allowed to touch).
fn repo_root(start: &std::path::Path) -> Option<PathBuf> {
    let mut dir = start.to_path_buf();
    loop {
        if dir.join(".git").exists() {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}

#[cfg(test)]
mod store_resolution_tests {
    use super::*;

    fn cfg(pinned: bool, store_dir: PathBuf) -> Config {
        Config {
            enabled: true,
            store_dir,
            inject_limit: 4000,
            store_dir_pinned: pinned,
        }
    }

    /// A project inside a repo resolves to that repo's tracked store, NOT the
    /// legacy global file. This is the whole point of the change: the queue has
    /// to live in the tree it describes so it can be committed with it.
    #[test]
    fn project_in_a_repo_resolves_to_the_repo_local_store() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("repo");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        let nested = root.join("crates").join("thing");
        std::fs::create_dir_all(&nested).unwrap();

        let c = cfg(false, PathBuf::from("/legacy"));
        assert_eq!(
            c.tasks_path_for(Some(nested.to_str().unwrap())),
            root.join(".backlog").join("tasks.toml"),
        );
    }

    /// `.git` is a FILE in a git worktree. Treating only directories as roots
    /// would send a worktree's tasks to the legacy store (or, worse, up into
    /// the main tree), so this pins the file form explicitly.
    #[test]
    fn a_worktree_whose_dot_git_is_a_file_is_still_a_root() {
        let tmp = tempfile::tempdir().unwrap();
        let wt = tmp.path().join("wt");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(wt.join(".git"), "gitdir: /elsewhere\n").unwrap();

        let c = cfg(false, PathBuf::from("/legacy"));
        assert_eq!(
            c.store_dir_for(Some(wt.to_str().unwrap())),
            wt.join(".backlog"),
        );
    }

    /// An operator who pinned `store_dir` keeps it. Per-project resolution is
    /// the default, not an override — silently relocating a pinned store would
    /// make their existing tasks vanish from every reader that looks there.
    #[test]
    fn a_pinned_store_dir_wins_over_project_resolution() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("repo");
        std::fs::create_dir_all(root.join(".git")).unwrap();

        let c = cfg(true, PathBuf::from("/pinned"));
        assert_eq!(
            c.store_dir_for(Some(root.to_str().unwrap())),
            PathBuf::from("/pinned"),
        );
    }

    /// A path with no `.git` anywhere above it cannot name a project store.
    /// Falling back to the legacy location keeps the tasks somewhere every
    /// existing reader already looks, instead of scattering them into whatever
    /// directory the caller happened to stand in.
    #[test]
    fn a_path_outside_any_repo_falls_back_to_the_legacy_store() {
        let tmp = tempfile::tempdir().unwrap();
        let orphan = tmp.path().join("no-repo-here");
        std::fs::create_dir_all(&orphan).unwrap();

        let legacy = PathBuf::from("/legacy");
        let c = cfg(false, legacy.clone());
        // tempdir() lives under the system temp root; only assert the fallback
        // when that root genuinely has no .git above it, so the test cannot
        // pass for the wrong reason on a machine whose temp dir sits in a repo.
        if repo_root(&orphan).is_none() {
            assert_eq!(c.store_dir_for(Some(orphan.to_str().unwrap())), legacy);
        }
    }
}
