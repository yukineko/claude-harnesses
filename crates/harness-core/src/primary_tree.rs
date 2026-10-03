//! "Is this path inside the PRIMARY working tree of a git repo?" — the
//! boundary behind backlog 1e6f00ae (CLAUDE.md §8: main's working tree only
//! receives merges, so a state-store WRITE that would land there is refused).
//!
//! Primary vs linked is git's own notion, not a path heuristic: in a linked
//! `git worktree`, `--git-dir` (`<main>/.git/worktrees/<name>`) differs from
//! `--git-common-dir` (`<main>/.git`); in a primary checkout they are equal.
//!
//! Three answers, not two (CLAUDE.md §3). [`TreeKind::Undetermined`] is what
//! you get when the path looks like it is inside a repo but git cannot be run,
//! refuses to answer, or answers something unparseable. [`refuse_if_primary`]
//! maps it to a refusal that names the reason; it is never read as "linked".
//! The only `Ok` outcomes are a confirmed linked worktree and a confirmed
//! absence of any repo (callers keep their own existing behaviour there).

use std::path::{Path, PathBuf};

use crate::git_probe;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeKind {
    /// No git repo above the path: git failed to run or to answer AND no `.git`
    /// entry exists above it (`git_probe::dot_git_present`). A `.git` entry with
    /// a failing git is `Undetermined`, never this.
    NoRepo,
    /// Inside the primary working tree (git-dir == git-common-dir).
    Primary,
    /// Inside a linked `git worktree` (git-dir != git-common-dir).
    Linked,
    /// Inside a `.git` directory itself (no working tree to be linked or not).
    InsideGitDir,
    /// Looks like a repo but the kind could not be determined; carries why.
    Undetermined(String),
}

/// Pure core: classify from git's two directory answers. `None` for either
/// (unparseable / not canonicalizable) is `Undetermined`, never a kind.
pub fn decide_from_dirs(git_dir: Option<&Path>, common_dir: Option<&Path>) -> TreeKind {
    match (git_dir, common_dir) {
        (Some(g), Some(c)) if g == c => TreeKind::Primary,
        (Some(_), Some(_)) => TreeKind::Linked,
        _ => TreeKind::Undetermined(
            "could not resolve --git-dir / --git-common-dir to comparable paths".into(),
        ),
    }
}

/// Nearest ancestor of `path` (inclusive) that exists on disk. A store dir that
/// has not been created yet is classified by where it WOULD be created.
fn nearest_existing(path: &Path) -> Option<PathBuf> {
    let mut cur = Some(path);
    while let Some(p) = cur {
        if p.exists() {
            return Some(p.to_path_buf());
        }
        cur = p.parent();
    }
    None
}

/// git's repository-LOCATION environment. Classification must depend only on
/// the path, never on the caller's env: git exports `GIT_DIR` (and friends) to
/// hooks, and `GIT_DIR=<linked gitdir>` with cwd in the primary tree used to
/// make a primary checkout classify as Linked. Every probe below clears these.
///
/// Deliberately NOT routed through [`git_probe::probe_repo`]: that probe is
/// shared by donegate/reviewgate/tdd-style gates which treat `GIT_DIR` as
/// *evidence of a repository* on purpose, so changing it would change their
/// semantics. This module needs the opposite (path-only), so it owns its probe.
const GIT_LOCATION_ENV: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_CEILING_DIRECTORIES",
    "GIT_NAMESPACE",
    "GIT_PREFIX",
    "GIT_DISCOVERY_ACROSS_FILESYSTEM",
    "GIT_INTERNAL_SUPER_PREFIX",
];

fn git_in(dir: &Path, args: &[&str]) -> std::io::Result<std::process::Output> {
    let mut c = std::process::Command::new("git");
    c.current_dir(dir).args(args);
    for v in GIT_LOCATION_ENV {
        c.env_remove(v);
    }
    c.output()
}

/// Exactly four trimmed lines, or `None` (anything else is not git's answer).
fn split_four_lines(stdout: &str) -> Option<[&str; 4]> {
    let v: Vec<&str> = stdout.lines().map(str::trim).collect();
    <[&str; 4]>::try_from(v).ok()
}

/// Classify `path` (which need not exist yet).
pub fn classify(path: &Path) -> TreeKind {
    classify_depth(path, 0)
}

fn classify_depth(path: &Path, depth: u8) -> TreeKind {
    if depth > 8 {
        return TreeKind::Undetermined("superproject chain deeper than 8".into());
    }
    let Some(dir) = nearest_existing(path) else {
        return TreeKind::Undetermined(format!("no ancestor of {} exists on disk", path.display()));
    };
    // A file path (e.g. an existing tasks.toml) is classified by its parent.
    let dir = if dir.is_dir() {
        dir
    } else {
        match dir.parent() {
            Some(p) => p.to_path_buf(),
            None => {
                return TreeKind::Undetermined(format!(
                    "{} has no parent directory",
                    dir.display()
                ));
            }
        }
    };
    // Filesystem evidence of a repo, independent of git and of env.
    let evidence = git_probe::dot_git_present(&dir);
    let out = match git_in(
        &dir,
        &[
            "rev-parse",
            "--is-inside-git-dir",
            "--is-inside-work-tree",
            "--path-format=absolute",
            "--git-dir",
            "--git-common-dir",
        ],
    ) {
        Ok(o) => o,
        Err(e) => {
            return if evidence {
                TreeKind::Undetermined(format!("could not run git: {e}"))
            } else {
                TreeKind::NoRepo
            };
        }
    };
    if !out.status.success() {
        return if evidence {
            TreeKind::Undetermined(format!(
                "git rev-parse failed ({}) although a .git entry exists above {}: {}",
                out.status,
                dir.display(),
                String::from_utf8_lossy(&out.stderr).trim()
            ))
        } else {
            TreeKind::NoRepo
        };
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let Some(lines) = split_four_lines(&stdout) else {
        return TreeKind::Undetermined(
            "git rev-parse printed something other than four lines".into(),
        );
    };
    match (lines[0], lines[1]) {
        ("true", _) => return TreeKind::InsideGitDir,
        ("false", "true") => {}
        ("false", "false") => {
            return TreeKind::Undetermined(
                "inside a repository that has no working tree (bare)".into(),
            );
        }
        _ => {
            return TreeKind::Undetermined(
                "git rev-parse answered neither true nor false for the location flags".into(),
            );
        }
    }
    let kind = decide_from_dirs(
        PathBuf::from(lines[2]).canonicalize().ok().as_deref(),
        PathBuf::from(lines[3]).canonicalize().ok().as_deref(),
    );
    if kind != TreeKind::Primary {
        return kind;
    }
    // git-dir == common-dir also holds for a submodule, whatever its
    // superproject is. A submodule inside a LINKED worktree is not primary:
    // classify the superproject's working tree instead.
    let sup = match git_in(&dir, &["rev-parse", "--show-superproject-working-tree"]) {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        Ok(o) => {
            return TreeKind::Undetermined(format!(
                "git rev-parse --show-superproject-working-tree failed ({})",
                o.status
            ));
        }
        Err(e) => return TreeKind::Undetermined(format!("could not run git: {e}")),
    };
    if sup.is_empty() {
        TreeKind::Primary
    } else {
        classify_depth(Path::new(&sup), depth + 1)
    }
}

/// `Ok(())` when a store write at `path` may proceed: confirmed linked worktree
/// or confirmed no repo. `Err(message)` for the primary working tree AND for an
/// undetermined classification (fail-closed, CLAUDE.md §3).
pub fn refuse_if_primary(path: &Path, what: &str) -> Result<(), String> {
    match classify(path) {
        TreeKind::Linked | TreeKind::NoRepo => Ok(()),
        TreeKind::Primary => Err(format!(
            "{what} REFUSED: {} is inside the primary working tree of a git repository. \
             main's working tree only receives merges (CLAUDE.md \u{a7}8); run this from a \
             linked worktree (git worktree add \u{2026}). Reads are still allowed here.",
            path.display()
        )),
        TreeKind::InsideGitDir => Err(format!(
            "{what} REFUSED: {} is inside a .git directory, not a linked worktree. \
             Run this from a linked worktree (git worktree add \u{2026}).",
            path.display()
        )),
        TreeKind::Undetermined(why) => Err(format!(
            "{what} REFUSED: could not determine whether {} is inside the primary working tree \
             of a git repository ({why}). An undetermined boundary is not a linked worktree \
             (CLAUDE.md \u{a7}3); fix the git problem or run from a linked worktree \
             (git worktree add \u{2026}).",
            path.display()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_four_lines_requires_exactly_four() {
        assert_eq!(
            split_four_lines("a\n b \nc\nd\n"),
            Some(["a", "b", "c", "d"])
        );
        assert_eq!(split_four_lines("a\nb\nc\n"), None);
        assert_eq!(split_four_lines("a\nb\nc\nd\ne\n"), None);
        assert_eq!(split_four_lines(""), None);
    }

    fn git(cwd: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args(args)
            .current_dir(cwd)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn repo() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let primary = base.join("primary");
        std::fs::create_dir_all(primary.join("sub")).unwrap();
        git(&primary, &["init", "-q", "-b", "main"]);
        std::fs::write(primary.join("f"), "x").unwrap();
        git(&primary, &["add", "-A"]);
        git(&primary, &["commit", "-q", "-m", "i"]);
        let linked = base.join("linked");
        git(
            &primary,
            &["worktree", "add", "-q", "-b", "f", linked.to_str().unwrap()],
        );
        (tmp, primary, linked)
    }

    #[test]
    fn decide_table() {
        let a = Path::new("/a/.git");
        let b = Path::new("/a/.git/worktrees/x");
        assert_eq!(decide_from_dirs(Some(a), Some(a)), TreeKind::Primary);
        assert_eq!(decide_from_dirs(Some(b), Some(a)), TreeKind::Linked);
        assert!(matches!(
            decide_from_dirs(None, Some(a)),
            TreeKind::Undetermined(_)
        ));
        assert!(matches!(
            decide_from_dirs(Some(a), None),
            TreeKind::Undetermined(_)
        ));
        assert!(matches!(
            decide_from_dirs(None, None),
            TreeKind::Undetermined(_)
        ));
    }

    #[test]
    fn real_git_primary_linked_and_subdir_and_not_yet_created() {
        let (_t, primary, linked) = repo();
        assert_eq!(classify(&primary), TreeKind::Primary);
        assert_eq!(classify(&primary.join("sub")), TreeKind::Primary);
        assert_eq!(
            classify(&primary.join(".backlog/tasks.toml")),
            TreeKind::Primary
        );
        assert_eq!(classify(&linked), TreeKind::Linked);
        assert_eq!(classify(&linked.join(".compass")), TreeKind::Linked);
        assert!(refuse_if_primary(&primary, "w")
            .unwrap_err()
            .contains("worktree"));
        assert!(refuse_if_primary(&linked, "w").is_ok());
    }

    #[test]
    fn no_repo_is_ok() {
        let t = tempfile::tempdir().unwrap();
        assert_eq!(classify(t.path()), TreeKind::NoRepo);
        assert!(refuse_if_primary(t.path(), "w").is_ok());
    }

    #[test]
    fn unreadable_repo_is_refused_not_allowed() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir(t.path().join(".git")).unwrap(); // looks like a repo, is not
        let k = classify(t.path());
        assert!(matches!(k, TreeKind::Undetermined(_)), "{k:?}");
        let e = refuse_if_primary(t.path(), "w").unwrap_err();
        assert!(e.contains("could not determine"), "{e}");
    }

    #[test]
    fn caller_git_env_does_not_change_the_answer() {
        let (_t, primary, linked) = repo();
        // Point GIT_DIR at the linked worktree's gitdir while standing in primary.
        let lg = std::process::Command::new("git")
            .args(["rev-parse", "--path-format=absolute", "--git-dir"])
            .current_dir(&linked)
            .output()
            .unwrap();
        let lg = String::from_utf8_lossy(&lg.stdout).trim().to_string();
        // Run the classifier in a child so the env mutation cannot race other tests.
        let exe = std::env::current_exe().unwrap();
        let out = std::process::Command::new(exe)
            .args(["--exact", "primary_tree::tests::env_child", "--nocapture"])
            .env("PT_CHILD_PATH", &primary)
            .env("GIT_DIR", &lg)
            .env("GIT_WORK_TREE", &primary)
            .output()
            .unwrap();
        let so = String::from_utf8_lossy(&out.stdout);
        assert!(
            so.contains("CHILD_KIND=Primary"),
            "{so}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[test]
    fn env_child() {
        if let Some(p) = std::env::var_os("PT_CHILD_PATH") {
            println!("CHILD_KIND={:?}", classify(Path::new(&p)));
        }
    }

    #[test]
    fn inside_git_dir_is_refused() {
        let (_t, primary, _l) = repo();
        assert_eq!(classify(&primary.join(".git")), TreeKind::InsideGitDir);
        assert!(refuse_if_primary(&primary.join(".git/.compass"), "w").is_err());
    }

    #[test]
    fn submodule_in_linked_worktree_is_linked_and_in_primary_is_primary() {
        let (t, primary, linked) = repo();
        let base = t.path().canonicalize().unwrap();
        let sub = base.join("subrepo");
        std::fs::create_dir_all(&sub).unwrap();
        git(&sub, &["init", "-q", "-b", "main"]);
        std::fs::write(sub.join("f"), "x").unwrap();
        git(&sub, &["add", "-A"]);
        git(&sub, &["commit", "-q", "-m", "i"]);
        git(
            &primary,
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                "-q",
                sub.to_str().unwrap(),
                "sm",
            ],
        );
        git(&primary, &["commit", "-q", "-m", "sm"]);
        git(&linked, &["merge", "-q", "main"]);
        git(
            &linked,
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "update",
                "--init",
                "-q",
            ],
        );
        assert_eq!(classify(&primary.join("sm")), TreeKind::Primary);
        assert_eq!(classify(&linked.join("sm")), TreeKind::Linked);
    }
}
