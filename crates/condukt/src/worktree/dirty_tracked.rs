//! Uncommitted tracked changes of a task worktree (backlog 1ca662ec), moved
//! verbatim out of `worktree.rs` (file-growth ratchet). `state set --status
//! verified` refuses a task whose worktree differs from HEAD.

use anyhow::{anyhow, bail, Result};
use std::path::Path;

use super::{ensure_own_worktree, git};

/// Tracked paths in the worktree at `path` whose working-tree or index state
/// differs from HEAD (staged or unstaged modifications, deletions, renames,
/// type changes, unmerged entries), excluding anything under `.backlog/`
/// (backlog 1ca662ec). Untracked and ignored files are NOT reported
/// (`--untracked-files=no`; ignored files never appear in porcelain output
/// without `--ignored`). An empty `Ok` therefore means "git observed HEAD ==
/// index == working tree for every tracked path outside `.backlog/`".
///
/// Fail-closed: `path` must be the top of its own worktree
/// ([`ensure_own_worktree`]) and `git status` must exit 0 (judged by exit
/// status via [`git`], not stdout); anything else is an `Err`, never an
/// empty list.
pub fn dirty_tracked_files(path: &Path) -> Result<Vec<String>> {
    ensure_own_worktree(path)
        .map_err(|e| anyhow!("status check failed for {}: {e}", path.display()))?;
    let raw = git(
        path,
        &["status", "--porcelain=v1", "-z", "--untracked-files=no"],
    )
    .map_err(|e| anyhow!("status check failed for {}: {e}", path.display()))?;
    parse_dirty_tracked(&raw)
}

/// `state set --status verified` committed-tree gate (backlog 1ca662ec). The
/// F→P proofs describe the task worktree's WORKING tree, but `worktree merge`
/// integrates only HEAD, so dirty tracked files ([`dirty_tracked_files`]) are
/// refused, named in the error; a git failure is refused too (fail-closed).
/// `wt == None` (no worktree passed or recorded) is NOT checked: there is no
/// task-owned tree to read, and the oracle's cwd fallback would judge the main
/// tree, whose changes can belong to other sessions (CLAUDE.md §8); verified
/// without a worktree is an established flow (many state-only tests).
pub fn refuse_uncommitted_task_worktree(task: &str, wt: Option<&str>) -> Result<()> {
    let Some(wt) = wt else { return Ok(()) };
    match dirty_tracked_files(Path::new(wt)) {
        Err(e) => bail!(
            "refusing to verify task '{task}': cannot determine whether its worktree {wt} \
             has uncommitted changes — {e:#}"
        ),
        Ok(d) if !d.is_empty() => bail!(
            "refusing to verify task '{task}': its worktree {wt} has uncommitted changes to \
             tracked files (a merge integrates HEAD, not the tested tree): {}",
            d.join(", ")
        ),
        Ok(_) => Ok(()),
    }
}

/// Parse `git status --porcelain=v1 -z` output into the dirty tracked paths,
/// dropping `.backlog/` paths. A rename/copy entry is followed by its source
/// path as a separate NUL field; both paths are considered. A malformed
/// record is an `Err` (an unparseable reading is not a clean one).
fn parse_dirty_tracked(raw: &str) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let mut fields = raw.split('\0').filter(|f| !f.is_empty());
    while let Some(rec) = fields.next() {
        let (Some(xy), Some(p)) = (rec.get(..2), rec.get(3..)) else {
            bail!("unparseable git status record {rec:?}");
        };
        if rec.as_bytes().get(2) != Some(&b' ') || p.is_empty() {
            bail!("unparseable git status record {rec:?}");
        }
        // Untracked/ignored never appear with `--untracked-files=no` and no
        // `--ignored`; skip defensively so they can never count as dirty.
        if xy == "??" || xy == "!!" {
            continue;
        }
        let mut paths = vec![p.to_string()];
        if xy.contains('R') || xy.contains('C') {
            match fields.next() {
                Some(src) => paths.push(src.to_string()),
                None => bail!("git status rename record {rec:?} has no source path"),
            }
        }
        for path in paths {
            if !(path == ".backlog" || path.starts_with(".backlog/")) && !out.contains(&path) {
                out.push(path);
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod dirty_tracked_tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn repo() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("r");
        fs::create_dir_all(dir.join(".backlog")).unwrap();
        git(&dir, &["init", "-q", "-b", "main"]).unwrap();
        git(&dir, &["config", "user.email", "t@t.t"]).unwrap();
        git(&dir, &["config", "user.name", "t"]).unwrap();
        fs::write(dir.join(".gitignore"), "ign/\n").unwrap();
        fs::write(dir.join("a.rs"), "a\n").unwrap();
        fs::write(dir.join("b.rs"), "b\n").unwrap();
        fs::write(dir.join(".backlog").join("x.json"), "{}\n").unwrap();
        git(&dir, &["add", "."]).unwrap();
        git(&dir, &["commit", "-q", "-m", "init"]).unwrap();
        (tmp, dir)
    }

    #[test]
    fn clean_tree_reports_nothing() {
        let (_t, d) = repo();
        assert_eq!(dirty_tracked_files(&d).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn untracked_ignored_and_backlog_changes_do_not_count() {
        let (_t, d) = repo();
        fs::write(d.join("new.rs"), "untracked\n").unwrap();
        fs::create_dir_all(d.join("ign")).unwrap();
        fs::write(d.join("ign").join("f"), "ignored\n").unwrap();
        fs::write(d.join(".backlog").join("x.json"), "{\"changed\":1}\n").unwrap();
        fs::write(d.join(".backlog").join("y.json"), "{}\n").unwrap();
        git(&d, &["add", ".backlog/y.json"]).unwrap();
        assert_eq!(dirty_tracked_files(&d).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn staged_unstaged_and_deleted_tracked_paths_are_named() {
        let (_t, d) = repo();
        fs::write(d.join("a.rs"), "unstaged\n").unwrap();
        fs::remove_file(d.join("b.rs")).unwrap();
        fs::write(d.join("c.rs"), "staged new\n").unwrap();
        git(&d, &["add", "c.rs"]).unwrap();
        let mut got = dirty_tracked_files(&d).unwrap();
        got.sort();
        assert_eq!(got, vec!["a.rs", "b.rs", "c.rs"]);
    }

    #[test]
    fn rename_names_both_paths() {
        let (_t, d) = repo();
        git(&d, &["mv", "a.rs", "z.rs"]).unwrap();
        let mut got = dirty_tracked_files(&d).unwrap();
        got.sort();
        assert_eq!(got, vec!["a.rs", "z.rs"]);
    }

    #[test]
    fn non_repo_and_missing_dirs_are_errors_not_clean() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(dirty_tracked_files(tmp.path()).is_err());
        assert!(dirty_tracked_files(&tmp.path().join("missing")).is_err());
    }

    #[test]
    fn subdirectory_of_a_repo_is_an_error() {
        let (_t, d) = repo();
        assert!(dirty_tracked_files(&d.join(".backlog")).is_err());
    }

    #[test]
    fn malformed_status_record_is_an_error() {
        assert!(parse_dirty_tracked("M").is_err());
        assert!(parse_dirty_tracked("R  new.rs\0").is_err());
    }
}
