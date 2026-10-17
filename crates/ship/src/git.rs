//! Read-only git detections for the ship checklist.
//!
//! Every detection returns a [`Determination`]: `Known(v)` only when the git
//! call ran AND exited successfully AND its output parsed; anything else —
//! spawn failure, non-zero exit, unparseable output, no upstream to compare
//! against — is `Undetermined`. A check that could not run must never render
//! as the all-clear value (CLAUDE.md §3).

use std::path::Path;
use std::process::Command;

use harness_core::verdict::Determination;

/// Run `git <args>` in `repo`; `Ok(stdout)` only on a successful exit.
fn git_stdout(repo: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .map_err(|e| format!("git {}: failed to spawn: {e}", args.join(" ")))?;
    if !output.status.success() {
        return Err(format!(
            "git {}: {} ({})",
            args.join(" "),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Check if there are uncommitted changes in the repository.
///
/// `Known(true)` iff `git status --porcelain` succeeded with non-empty output
/// (staged, unstaged, or untracked changes). A failed `git status` is
/// `Undetermined`, never `Known(false)`.
pub fn uncommitted_changes(repo: &Path) -> Determination<bool> {
    match git_stdout(repo, &["status", "--porcelain"]) {
        Ok(out) => Determination::known(!out.trim().is_empty()),
        Err(e) => Determination::undetermined(e),
    }
}

/// Get local branches matching `condukt/*` that are not merged into `main`.
///
/// Uses `git branch --no-merged main --list "condukt/*" --format "%(refname:short)"`.
/// A failed command (e.g. no `main` branch) is `Undetermined`, never an empty list.
pub fn unmerged_condukt_branches(repo: &Path) -> Determination<Vec<String>> {
    match git_stdout(
        repo,
        &[
            "branch",
            "--no-merged",
            "main",
            "--list",
            "condukt/*",
            "--format",
            "%(refname:short)",
        ],
    ) {
        Ok(out) => Determination::known(
            out.lines()
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .collect(),
        ),
        Err(e) => Determination::undetermined(e),
    }
}

/// Get worktree paths other than the main repo path.
///
/// Parses `git worktree list --porcelain` and collects paths that are not the
/// main repo. A failed command or an uncanonicalizable repo path is
/// `Undetermined`. A listed worktree whose path cannot be canonicalized (e.g.
/// its directory was deleted but the worktree was never pruned) is still a
/// leftover and is reported, not dropped.
pub fn leftover_worktrees(repo: &Path) -> Determination<Vec<String>> {
    let main_repo_path = match std::fs::canonicalize(repo) {
        Ok(p) => p,
        Err(e) => {
            return Determination::undetermined(format!(
                "cannot canonicalize repo path {}: {e}",
                repo.display()
            ))
        }
    };

    let stdout = match git_stdout(repo, &["worktree", "list", "--porcelain"]) {
        Ok(s) => s,
        Err(e) => return Determination::undetermined(e),
    };

    let mut result = vec![];
    for line in stdout.lines() {
        if let Some(path_str) = line.strip_prefix("worktree ") {
            match std::fs::canonicalize(path_str) {
                Ok(canonical_path) if canonical_path == main_repo_path => {}
                _ => result.push(path_str.to_string()),
            }
        }
    }
    Determination::known(result)
}

/// Count commits ahead of upstream.
///
/// Tries `git rev-list --count @{upstream}..HEAD`. If that fails (no upstream
/// configured), falls back to `git rev-list --count origin/main..HEAD`. If
/// that also fails, the count is `Undetermined` — with nothing to compare
/// against, "0 unpushed" is not an observation.
pub fn unpushed_count(repo: &Path) -> Determination<usize> {
    let mut errors = vec![];
    for range in ["@{upstream}..HEAD", "origin/main..HEAD"] {
        match git_stdout(repo, &["rev-list", "--count", range]) {
            Ok(out) => match out.trim().parse::<usize>() {
                Ok(count) => return Determination::known(count),
                Err(e) => errors.push(format!("git rev-list --count {range}: {e}")),
            },
            Err(e) => errors.push(e),
        }
    }
    Determination::undetermined(errors.join("; "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn setup_git_repo() -> TempDir {
        let dir = TempDir::new().unwrap();
        let repo_path = dir.path();

        // Initialize git repo on `main` (the branch the detections compare to)
        Command::new("git")
            .arg("init")
            .arg("-b")
            .arg("main")
            .current_dir(repo_path)
            .output()
            .unwrap();

        // Set user config
        Command::new("git")
            .arg("config")
            .arg("user.email")
            .arg("t@t")
            .current_dir(repo_path)
            .output()
            .unwrap();

        Command::new("git")
            .arg("config")
            .arg("user.name")
            .arg("t")
            .current_dir(repo_path)
            .output()
            .unwrap();

        dir
    }

    fn commit_file(repo_path: &Path, name: &str) {
        fs::write(repo_path.join(name), "content").unwrap();
        Command::new("git")
            .arg("add")
            .arg(name)
            .current_dir(repo_path)
            .output()
            .unwrap();
        Command::new("git")
            .arg("-c")
            .arg("user.email=t@t")
            .arg("-c")
            .arg("user.name=t")
            .arg("commit")
            .arg("-m")
            .arg("commit")
            .current_dir(repo_path)
            .output()
            .unwrap();
    }

    #[test]
    fn test_uncommitted_changes() {
        let dir = setup_git_repo();
        let repo_path = dir.path();

        // Initially no changes
        assert_eq!(uncommitted_changes(repo_path), Determination::Known(false));

        // Create an untracked file
        fs::write(repo_path.join("untracked.txt"), "content").unwrap();

        // Now there should be uncommitted changes
        assert_eq!(uncommitted_changes(repo_path), Determination::Known(true));

        // Add and commit
        commit_file(repo_path, "untracked.txt");

        // After commit, no more uncommitted changes
        assert_eq!(uncommitted_changes(repo_path), Determination::Known(false));
    }

    #[test]
    fn test_uncommitted_changes_not_a_repo_is_undetermined() {
        let dir = TempDir::new().unwrap();
        assert!(matches!(
            uncommitted_changes(dir.path()),
            Determination::Undetermined(_)
        ));
    }

    /// With no upstream and no origin/main there is nothing to compare HEAD
    /// against, so the unpushed count cannot be determined. It used to return
    /// a fail-open 0 ("nothing unpushed"); CLAUDE.md §3 requires Undetermined.
    #[test]
    fn test_unpushed_count_no_upstream() {
        let dir = setup_git_repo();
        let repo_path = dir.path();

        commit_file(repo_path, "file.txt");

        let count = unpushed_count(repo_path);
        assert!(
            matches!(count, Determination::Undetermined(_)),
            "no upstream must be Undetermined, got {count:?}"
        );
    }

    #[test]
    fn test_leftover_worktrees_empty() {
        let dir = setup_git_repo();
        let repo_path = dir.path();

        // In a fresh repo with no worktrees, should return empty
        assert_eq!(leftover_worktrees(repo_path), Determination::Known(vec![]));
    }

    #[test]
    fn test_unmerged_condukt_branches_empty() {
        let dir = setup_git_repo();
        let repo_path = dir.path();

        commit_file(repo_path, "file.txt");

        // In a fresh repo with no unmerged condukt branches, should return empty
        assert_eq!(
            unmerged_condukt_branches(repo_path),
            Determination::Known(vec![])
        );
    }

    #[test]
    fn test_unmerged_condukt_branches_no_main_is_undetermined() {
        let dir = TempDir::new().unwrap();
        Command::new("git")
            .arg("init")
            .arg("-b")
            .arg("trunk")
            .current_dir(dir.path())
            .output()
            .unwrap();
        commit_file(dir.path(), "file.txt");
        assert!(matches!(
            unmerged_condukt_branches(dir.path()),
            Determination::Undetermined(_)
        ));
    }
}
