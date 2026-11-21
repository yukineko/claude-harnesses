//! Group resolution (SPEC §2): the git common dir of the repo containing a
//! directory, found with the filesystem only (no `git` subprocess), and the
//! group key derived from it.
//!
//! Every failure is an `Err(reason)` — a cannot-determine. There is no
//! "default group": two sessions that could not be placed must not be
//! assumed to be alone (which would make each of them master).

use harness_core::verdict::Required;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Where a directory's repository lives.
#[derive(Debug, Clone)]
pub struct GitLoc {
    /// The per-worktree git dir (holds `HEAD`).
    pub git_dir: PathBuf,
    /// The shared common dir (same for every worktree of one repo).
    pub common_dir: PathBuf,
}

/// A resolved session group.
#[derive(Debug, Clone)]
pub struct Group {
    pub key: String,
    pub common_dir: String,
}

/// Read a file: `Ok(None)` when absent, `Err` when it exists but cannot be read.
pub fn read_opt(path: &Path) -> Result<Option<String>, String> {
    match harness_core::boundary::read_to_string(path).require() {
        Required::Determined(v) => Ok(v),
        Required::Blocked(why) => Err(why.as_str().to_string()),
    }
}

fn absolutize(p: &Path) -> Result<PathBuf, String> {
    if p.is_absolute() {
        return Ok(p.to_path_buf());
    }
    match std::env::current_dir() {
        Ok(c) => Ok(c.join(p)),
        Err(e) => Err(format!("カレントディレクトリを取得できない ({e})")),
    }
}

/// Walk up from `start` to the first `.git` and resolve git dir / common dir.
pub fn locate(start: &Path) -> Result<GitLoc, String> {
    let start = absolutize(start)?;
    let mut dir: Option<&Path> = Some(start.as_path());
    while let Some(d) = dir {
        let dotgit = d.join(".git");
        match std::fs::metadata(&dotgit) {
            Ok(m) if m.is_dir() => {
                return Ok(GitLoc {
                    git_dir: dotgit.clone(),
                    common_dir: dotgit,
                });
            }
            Ok(_) => return from_gitfile(&dotgit, d),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(format!("{} を読めない ({e})", dotgit.display()));
            }
        }
        dir = d.parent();
    }
    Err(format!(
        "{} から上に git repo が見つからない",
        start.display()
    ))
}

fn from_gitfile(dotgit: &Path, parent: &Path) -> Result<GitLoc, String> {
    let text = match read_opt(dotgit)? {
        Some(t) => t,
        None => return Err(format!("{} が消えた", dotgit.display())),
    };
    let line = text.lines().next().map(str::trim);
    let p = match line.and_then(|l| l.strip_prefix("gitdir:")) {
        Some(p) => p.trim(),
        None => return Err(format!("{} に gitdir: 行が無い", dotgit.display())),
    };
    if p.is_empty() {
        return Err(format!("{} の gitdir が空", dotgit.display()));
    }
    let git_dir = {
        let pp = Path::new(p);
        if pp.is_absolute() {
            pp.to_path_buf()
        } else {
            parent.join(pp)
        }
    };
    let common_dir = match read_opt(&git_dir.join("commondir"))? {
        Some(c) => {
            let c = c.trim();
            if c.is_empty() {
                return Err(format!("{}/commondir が空", git_dir.display()));
            }
            let cp = Path::new(c);
            if cp.is_absolute() {
                cp.to_path_buf()
            } else {
                git_dir.join(cp)
            }
        }
        None => git_dir.clone(),
    };
    Ok(GitLoc {
        git_dir,
        common_dir,
    })
}

/// Group key = first 16 hex of sha256(canonical common dir).
pub fn resolve(cwd: &Path) -> Result<Group, String> {
    let loc = locate(cwd)?;
    let canon = std::fs::canonicalize(&loc.common_dir).map_err(|e| {
        format!(
            "common dir {} を canonicalize できない ({e})",
            loc.common_dir.display()
        )
    })?;
    let s = match canon.to_str() {
        Some(s) => s.to_string(),
        None => return Err(format!("common dir {} が UTF-8 でない", canon.display())),
    };
    Ok(Group {
        key: key_of(&s),
        common_dir: s,
    })
}

pub fn key_of(common_dir: &str) -> String {
    let digest = Sha256::digest(common_dir.as_bytes());
    let mut out = String::with_capacity(16);
    for b in digest.iter().take(8) {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// The branch checked out in the worktree containing `dir`:
/// `Ok(Some(branch))`, `Ok(None)` for a detached HEAD, `Err` when unreadable.
pub fn head_branch(dir: &Path) -> Result<Option<String>, String> {
    let loc = locate(dir)?;
    let head = loc.git_dir.join("HEAD");
    let text = match read_opt(&head)? {
        Some(t) => t,
        None => return Err(format!("{} が無い", head.display())),
    };
    let t = text.trim();
    match t.strip_prefix("ref:") {
        Some(r) => match r.trim().strip_prefix("refs/heads/") {
            Some(b) => Ok(Some(b.to_string())),
            None => Err(format!("{} の ref {r:?} を解釈できない", head.display())),
        },
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_is_16_hex() {
        let k = key_of("/x/.git");
        assert_eq!(k.len(), 16);
        assert!(k.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
