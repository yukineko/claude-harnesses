//! Durable "which repository did this linked worktree belong to" records.
//!
//! [`crate::session_roots`] attributes every file a session wrote to the git
//! checkout containing it. Once a linked worktree is removed (`git worktree
//! remove`, condukt teardown), git can no longer say which repository it
//! belonged to: the directory, and with it the `.git` gitfile, is gone. This
//! module keeps an append-only JSONL store of `{worktree, repo}` pairs
//! (canonical linked-worktree toplevel, canonical main-repository toplevel),
//! written while the worktree still exists, so a later lookup can still
//! attribute the write to its repository.
//!
//! Limitation: a pair exists only if something recorded it while the worktree
//! existed: a `session_roots` resolve that saw a write into it, or the repo's
//! `.githooks/post-checkout` hook (run by `git worktree add` inside the new
//! worktree), which appends the same line format to the same store path.
//! A worktree removed before any such resolve has no record, and the lookup
//! answers [`Lookup::NoRecord`]; [`crate::session_roots`] then judges the
//! written path by its nearest existing ancestor (excluded when git positively
//! reports that ancestor outside every repository, attributed to the repository
//! containing it, `Undetermined` on any other failure).
//!
//! Store location: `$HARNESS_WORKTREE_ORIGIN_DIR/worktree-origins.jsonl` when
//! that variable is an absolute path (a relative or empty value is ignored),
//! else `<home>/.harness/worktree-origins.jsonl` with `<home>` from
//! [`crate::config::home`].

use std::path::{Path, PathBuf};
use std::process::Command;

/// Env var overriding the store directory (absolute paths only).
pub const DIR_ENV: &str = "HARNESS_WORKTREE_ORIGIN_DIR";
/// File name of the store inside its directory.
pub const FILE_NAME: &str = "worktree-origins.jsonl";

/// Default store path (see the module docs).
pub fn default_store() -> PathBuf {
    let dir = std::env::var(DIR_ENV)
        .ok()
        .filter(|s| !s.is_empty())
        .filter(|s| Path::new(s).is_absolute())
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::config::home().join(".harness"));
    dir.join(FILE_NAME)
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct Record {
    worktree: PathBuf,
    repo: PathBuf,
}

/// If `toplevel` (a canonical git toplevel) is a linked worktree, return the
/// canonical toplevel of the repository whose common dir it shares.
///
/// `Ok(None)`: not a linked worktree (git dir equals common dir), or the
/// common dir does not sit inside a non-bare working tree (nothing to record).
/// `Err`: git could not answer.
pub fn linked_origin(toplevel: &Path) -> Result<Option<PathBuf>, String> {
    let mut cmd = Command::new("git");
    crate::session_roots::clear_git_discovery_env(&mut cmd);
    let out = cmd
        .arg("-C")
        .arg(toplevel)
        .args([
            "rev-parse",
            "--path-format=absolute",
            "--git-dir",
            "--git-common-dir",
        ])
        .output()
        .map_err(|e| format!("cannot run git in {}: {e}", toplevel.display()))?;
    if !out.status.success() {
        return Err(format!(
            "git rev-parse --git-dir --git-common-dir failed in {} (exit {:?}): {}",
            toplevel.display(),
            out.status.code(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let mut lines = stdout.lines();
    let (Some(git_dir), Some(common), None) = (lines.next(), lines.next(), lines.next()) else {
        return Err(format!(
            "git rev-parse printed an unexpected answer in {}: {stdout:?}",
            toplevel.display()
        ));
    };
    let canon =
        |s: &str| std::fs::canonicalize(s).map_err(|e| format!("cannot canonicalize {s}: {e}"));
    let git_dir = canon(git_dir)?;
    let common = canon(common)?;
    if git_dir == common {
        return Ok(None);
    }
    let Some(parent) = common.parent() else {
        return Ok(None);
    };
    // The parent of the common dir is the main repository only if git itself
    // reports it as its own toplevel (a bare repository's parent is not).
    let mut cmd = Command::new("git");
    crate::session_roots::clear_git_discovery_env(&mut cmd);
    let out = cmd
        .arg("-C")
        .arg(parent)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map_err(|e| format!("cannot run git in {}: {e}", parent.display()))?;
    if !out.status.success() {
        return Ok(None);
    }
    let top = String::from_utf8_lossy(&out.stdout);
    match std::fs::canonicalize(top.trim()) {
        Ok(t) if t == parent => Ok(Some(t)),
        _ => Ok(None),
    }
}

/// Append the pair `worktree -> repo` to `store` unless an identical record is
/// already present. One single-`write()` line per record (see
/// [`crate::append::append_line`]), safe under concurrent appenders.
pub fn record(store: &Path, worktree: &Path, repo: &Path) -> std::io::Result<()> {
    let rec = Record {
        worktree: worktree.to_path_buf(),
        repo: repo.to_path_buf(),
    };
    // Duplicate check is an optimisation only: if the store cannot be read or
    // parsed, append anyway (a duplicate line is harmless to the lookup).
    if let Ok(existing) = read_all(store) {
        if existing.contains(&rec) {
            return Ok(());
        }
    }
    let line = serde_json::to_string(&rec).map_err(std::io::Error::other)?;
    crate::append::append_line(store, &line)
}

/// Every record in `store`. A missing store is an empty list; any other read
/// error or an unparsable non-empty line is an error.
fn read_all(store: &Path) -> Result<Vec<Record>, String> {
    let text = match std::fs::read_to_string(store) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => {
            return Err(format!(
                "cannot read the worktree-origin store {}: {e}",
                store.display()
            ))
        }
    };
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let rec: Record = serde_json::from_str(line).map_err(|e| {
            format!(
                "the worktree-origin store {} has an unparsable line {}: {e}",
                store.display(),
                i + 1
            )
        })?;
        out.push(rec);
    }
    Ok(out)
}

/// Result of [`lookup`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup {
    /// No recorded worktree contains the path (or the store does not exist).
    NoRecord,
    /// The longest recorded worktree containing the path, and its repository.
    Found { worktree: PathBuf, repo: PathBuf },
}

/// `path` with its longest existing ancestor canonicalized and the missing
/// remainder re-appended, so a path spelled through a symlink (`/var` vs
/// `/private/var`) still compares against the canonical recorded worktree.
fn canonical_prefix(path: &Path) -> PathBuf {
    let mut rest: Vec<&std::ffi::OsStr> = Vec::new();
    let mut cur = path;
    loop {
        if let Ok(c) = std::fs::canonicalize(cur) {
            let mut out = c;
            for comp in rest.iter().rev() {
                out.push(comp);
            }
            return out;
        }
        match (cur.parent(), cur.file_name()) {
            (Some(p), Some(name)) => {
                rest.push(name);
                cur = p;
            }
            _ => return path.to_path_buf(),
        }
    }
}

/// Find the longest recorded worktree that is a component-wise prefix of
/// `written`. `Err` when the store exists but cannot be read or parsed.
pub fn lookup(store: &Path, written: &Path) -> Result<Lookup, String> {
    let records = read_all(store)?;
    let candidates = [written.to_path_buf(), canonical_prefix(written)];
    let best = records
        .into_iter()
        .filter(|r| candidates.iter().any(|c| c.starts_with(&r.worktree)))
        .max_by_key(|r| r.worktree.components().count());
    Ok(match best {
        Some(r) => Lookup::Found {
            worktree: r.worktree,
            repo: r.repo,
        },
        None => Lookup::NoRecord,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn missing_store_is_no_record() {
        let d = tempfile::tempdir().unwrap();
        let store = d.path().join("none.jsonl");
        assert_eq!(
            lookup(&store, Path::new("/a/b/c")).unwrap(),
            Lookup::NoRecord
        );
    }

    #[test]
    fn corrupt_store_is_an_error() {
        let d = tempfile::tempdir().unwrap();
        let store = d.path().join(FILE_NAME);
        std::fs::write(&store, "{\"worktree\":\"/a\",\"repo\":\"/r\"}\nnot json\n").unwrap();
        let e = lookup(&store, Path::new("/a/b")).unwrap_err();
        assert!(e.contains("unparsable line 2"), "{e}");
    }

    #[test]
    fn unreadable_store_is_an_error() {
        let d = tempfile::tempdir().unwrap();
        // A directory where the store file should be: read fails, not NotFound.
        let store = d.path().join(FILE_NAME);
        std::fs::create_dir(&store).unwrap();
        assert!(lookup(&store, Path::new("/a/b")).is_err());
    }

    #[test]
    fn longest_prefix_wins_and_is_component_wise() {
        let d = tempfile::tempdir().unwrap();
        let store = d.path().join(FILE_NAME);
        record(&store, Path::new("/w"), Path::new("/r1")).unwrap();
        record(&store, Path::new("/w/inner"), Path::new("/r2")).unwrap();
        record(&store, Path::new("/x"), Path::new("/r3")).unwrap();
        assert_eq!(
            lookup(&store, Path::new("/w/inner/f.rs")).unwrap(),
            Lookup::Found {
                worktree: "/w/inner".into(),
                repo: "/r2".into()
            }
        );
        assert_eq!(
            lookup(&store, Path::new("/w/other/f.rs")).unwrap(),
            Lookup::Found {
                worktree: "/w".into(),
                repo: "/r1".into()
            }
        );
        // `/xy` is not under `/x` component-wise.
        assert_eq!(
            lookup(&store, Path::new("/xy/f.rs")).unwrap(),
            Lookup::NoRecord
        );
    }

    #[test]
    fn record_skips_exact_duplicates() {
        let d = tempfile::tempdir().unwrap();
        let store = d.path().join("sub").join(FILE_NAME);
        record(&store, Path::new("/w"), Path::new("/r")).unwrap();
        record(&store, Path::new("/w"), Path::new("/r")).unwrap();
        let text = std::fs::read_to_string(&store).unwrap();
        assert_eq!(text.lines().count(), 1, "{text}");
    }

    #[test]
    fn linked_origin_detects_linked_worktree_and_not_main() {
        let d = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(d.path()).unwrap();
        let repo = base.join("repo");
        let wt = base.join("wt");
        std::fs::create_dir_all(&repo).unwrap();
        let git = |dir: &Path, args: &[&str]| {
            let o = Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(args)
                .output()
                .unwrap();
            assert!(o.status.success(), "{o:?}");
        };
        git(&repo, &["init", "-q"]);
        git(
            &repo,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@e",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "i",
            ],
        );
        git(
            &repo,
            &["worktree", "add", "-q", "-b", "w", wt.to_str().unwrap()],
        );
        assert_eq!(linked_origin(&repo).unwrap(), None);
        assert_eq!(linked_origin(&wt).unwrap(), Some(repo));
    }
}
