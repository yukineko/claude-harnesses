use std::path::{Path, PathBuf};

use harness_core::config::{base_dir, expand_tilde};
use serde::Deserialize;

pub struct Config {
    pub enabled: bool,
    pub store_dir: PathBuf,
    pub inject_limit: usize,
    /// True when `config.toml` named `store_dir` explicitly. An operator who
    /// pinned a location keeps it: per-project resolution is a DEFAULT, not an
    /// override, so `locate` must not silently relocate their store.
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

    /// The store for one checkout: `<repo root>/.backlog/tasks.toml`.
    ///
    /// backlog is cross-project, so a single global file merged every
    /// project's queue into one path that no repo could review, commit, or
    /// ship with the work it describes. Resolving the store per repo keeps
    /// each queue inside the tree it talks about, where it is an ordinary
    /// tracked file.
    ///
    /// `start` is the path the ancestor scan begins from — it is NOT a
    /// "which project's queue do I want to read" selector. Callers must pass
    /// a path belonging to their OWN checkout, or `None` to mean the cwd:
    ///   - the CLI (`main.rs`) always passes `None`. It deliberately does not
    ///     forward `--project`: a worktree running `add --project <main tree>`
    ///     would otherwise resolve the store to the main tree and write that
    ///     tree's tracked `tasks.toml`, which CLAUDE.md §8 forbids.
    ///   - the SessionStart hook passes `Some(root)` derived from the hook's
    ///     own cwd, which is the same cwd anchoring, spelled explicitly.
    ///
    /// Resolution order, and what each step means:
    ///   1. `store_dir` pinned in `config.toml` — an operator who named a
    ///      location keeps it. Per-repo resolution is the DEFAULT, not an
    ///      override. A pinned store is explicitly allowed to hold SEVERAL
    ///      projects, which is why readers keep scoping it by project label.
    ///   2. the repo root containing `start` (or the cwd when `start` is
    ///      `None`) — `<root>/.backlog`. This is a project store: everything
    ///      in it belongs to that repo, so the file itself is the scope.
    ///   3. no repo root found — [`StoreLocation::NoProject`], which carries
    ///      NO path. See its doc comment for why this stopped being a fallback
    ///      to `~/.backlog`.
    pub fn locate(&self, start: Option<&str>) -> StoreLocation {
        if self.store_dir_pinned {
            return StoreLocation::Pinned(self.store_dir.clone());
        }
        let start = start
            .map(PathBuf::from)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));
        match repo_root(&start) {
            Some(root) => StoreLocation::Repo {
                dir: root.join(".backlog"),
                root,
            },
            None => StoreLocation::NoProject { start },
        }
    }

    pub fn disabled_env() -> bool {
        std::env::var("BACKLOG_DISABLE")
            .map(|v| v == "1")
            .unwrap_or(false)
    }
}

/// Where this process's queue lives — three answers, not one path.
///
/// The last two variants used to be a single `PathBuf`: a cwd with no repo root
/// above it silently resolved to the cross-project `~/.backlog`, and every
/// caller then treated that exactly like a project store. Two consequences were
/// measured before this type existed:
///
///   * a process running in a tempdir wrote its fixtures into the operator's
///     real queue — specforge's spec-ratify tasks were found in
///     `~/.backlog/tasks.toml` under `project="/tmp/.tmpsYSvwG"` and
///     `project="/tmp/.tmpWIjVKj"` (backlog 7d8ab7fe; the same class on the
///     other machine is a4966045, 6 entries);
///   * a read from such a cwd answered with ANOTHER project's queue, which is
///     the collapse CLAUDE.md §3 forbids one level below 5ba13c3e: "I have no
///     store for you" rendered as an ordinary answer.
///
/// "There is no project store here" is not a location. Substituting a shared
/// one is not a degrade, it is a different question being answered, so
/// [`StoreLocation::NoProject`] deliberately carries no path and
/// [`StoreLocation::tasks_path`] refuses.
pub enum StoreLocation {
    /// `config.toml` named `store_dir`. Used verbatim, and explicitly allowed
    /// to hold several projects — the pre-per-repo shape, kept as the opt-in
    /// escape hatch the `NoProject` diagnostic points at.
    Pinned(PathBuf),
    /// `<root>/.backlog` for the repo containing the starting path. `root` is
    /// the SCOPE: a tracked, repo-local file holds that repo's tasks and
    /// nothing else, whichever checkout wrote them.
    Repo { root: PathBuf, dir: PathBuf },
    /// No pinned `store_dir` and no repo root above `start`. Carries no path
    /// on purpose.
    NoProject { start: PathBuf },
}

impl StoreLocation {
    /// The repo this store belongs to, when the store IS a project store.
    ///
    /// `None` for [`StoreLocation::Pinned`] — a pinned store may hold several
    /// projects, so there the project label is the scope and callers must keep
    /// filtering by it — and for [`StoreLocation::NoProject`], which has no
    /// store at all. Callers therefore cannot use "no root" to mean "no
    /// scoping needed": the two `None` cases are distinguished by matching on
    /// the variant, and `tasks_path` refuses the second outright.
    pub fn project_root(&self) -> Option<&Path> {
        match self {
            StoreLocation::Repo { root, .. } => Some(root),
            StoreLocation::Pinned(_) | StoreLocation::NoProject { .. } => None,
        }
    }

    /// The `tasks.toml` to read/write, or the reason there is none.
    ///
    /// Returns `Err` for [`StoreLocation::NoProject`] rather than a path, which
    /// is the whole point of the type: callers used to receive `~/.backlog`
    /// here and could not tell it apart from their own project's store.
    pub fn tasks_path(&self) -> Result<PathBuf, String> {
        match self {
            StoreLocation::Pinned(dir) => Ok(dir.join("tasks.toml")),
            StoreLocation::Repo { dir, .. } => Ok(dir.join("tasks.toml")),
            StoreLocation::NoProject { start } => Err(format!(
                "no git repo above {}, so there is no project store to read or write. \
                 backlog resolves its queue per repo (<repo root>/.backlog/tasks.toml) and \
                 refuses to fall back to the cross-project ~/.backlog: that makes another \
                 project's queue look like this one's, and it is how tempdir runs wrote \
                 their fixtures into the real queue (backlog 7d8ab7fe). Run from inside the \
                 repo whose queue you mean, or pin `store_dir` in ~/.backlog/config.toml to \
                 opt into a shared store.",
                start.display()
            )),
        }
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
///
/// `pub(crate)`: also used by `store::canonical_project_id`, which needs the
/// IDENTICAL ancestor scan to decide project *identity* (as opposed to this
/// module's use of it to decide the store's *location*) — see that function's
/// doc comment for why the two are deliberately kept as separate concerns
/// sharing one scan.
pub(crate) fn repo_root(start: &std::path::Path) -> Option<PathBuf> {
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
        let loc = c.locate(Some(nested.to_str().unwrap()));
        assert_eq!(
            loc.tasks_path().unwrap(),
            root.join(".backlog").join("tasks.toml"),
        );
        // The repo is also reported as the SCOPE, which is what lets readers
        // stop filtering the file by project label.
        assert_eq!(loc.project_root(), Some(root.as_path()));
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
        let loc = c.locate(Some(wt.to_str().unwrap()));
        assert_eq!(
            loc.tasks_path().unwrap(),
            wt.join(".backlog").join("tasks.toml")
        );
        assert_eq!(loc.project_root(), Some(wt.as_path()));
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
        let loc = c.locate(Some(root.to_str().unwrap()));
        assert_eq!(
            loc.tasks_path().unwrap(),
            PathBuf::from("/pinned").join("tasks.toml"),
        );
        // A pinned store reports NO project root: it may hold several
        // projects, so its readers must keep scoping by project label.
        assert_eq!(loc.project_root(), None);
    }

    /// A path with no `.git` anywhere above it cannot name a project store,
    /// and there is no answer to substitute. This inverts the previous
    /// contract, which fell back to the cross-project legacy file: that
    /// fallback is how a tempdir run's fixtures reached the operator's real
    /// queue (backlog 7d8ab7fe) and how a read from such a cwd was answered
    /// with another project's work.
    #[test]
    fn a_path_outside_any_repo_is_not_a_store() {
        let tmp = tempfile::tempdir().unwrap();
        let orphan = tmp.path().join("no-repo-here");
        std::fs::create_dir_all(&orphan).unwrap();

        let c = cfg(false, PathBuf::from("/legacy"));
        // tempdir() lives under the system temp root; a machine whose temp dir
        // sits inside a repo cannot observe this at all, so fail loudly there
        // instead of reporting green on a case that never ran.
        assert!(
            repo_root(&orphan).is_none(),
            "{} is inside a git repo, so this case cannot be observed here",
            orphan.display()
        );
        let loc = c.locate(Some(orphan.to_str().unwrap()));
        assert_eq!(loc.project_root(), None);
        let err = loc
            .tasks_path()
            .expect_err("a path outside any repo must not resolve to a store");
        // The diagnostic has to name the escape hatch, or the refusal is a
        // dead end for the operator who genuinely wants a shared store.
        assert!(err.contains("store_dir"), "err={err}");
        assert!(err.contains(&orphan.display().to_string()), "err={err}");
    }

    /// The refusal must not leak into the PINNED case: an operator who named a
    /// store keeps it even from a cwd outside any repo. This is the control
    /// that stops the change above from being a blanket removal.
    #[test]
    fn a_pinned_store_dir_still_resolves_outside_any_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let orphan = tmp.path().join("no-repo-here");
        std::fs::create_dir_all(&orphan).unwrap();
        assert!(repo_root(&orphan).is_none(), "temp dir is inside a repo");

        let c = cfg(true, PathBuf::from("/pinned"));
        assert_eq!(
            c.locate(Some(orphan.to_str().unwrap()))
                .tasks_path()
                .unwrap(),
            PathBuf::from("/pinned").join("tasks.toml"),
        );
    }
}
