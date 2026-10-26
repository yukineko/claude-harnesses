//! The set of checkouts a Stop gate must judge.
//!
//! A gate that uses only the hook `cwd` judges the wrong tree when the session
//! worked in a linked git worktree while the hook ran in the primary tree: it
//! sees a clean tree and allows. The roots here are the hook cwd plus the git
//! toplevel of every file the session transcript records as written.
//!
//! Fail-closed (CLAUDE.md section 3): an unreadable transcript, a relative
//! written path, a written file whose directory is gone or unenterable, or any
//! git failure other than the one below is `Undetermined` naming the path.
//! The single exclusion: an absolute written path whose existing parent
//! directory git positively reports as outside every repository (exit 128 and
//! the full "not a git repository (or any of the parent directories)" message)
//! is skipped, not an error. It still counts toward `wrote_any`.
//! The git child runs with repository-discovery env vars (GIT_CEILING_DIRECTORIES,
//! GIT_DIR, ...) removed, so inherited env cannot make an in-repo path look
//! outside every repository.
//! Bash-derived candidates are heuristic (see
//! [`crate::attribution::SessionWrites::bash`]): a candidate outside any git
//! work tree is not a root and is not an error.

use crate::verdict::Determination;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRoots {
    /// Canonical, deduplicated, hook cwd first.
    pub roots: Vec<PathBuf>,
    /// The transcript records at least one written file.
    pub wrote_any: bool,
}

/// Why a toplevel could not be resolved. `OutsideAnyRepo` is a positive
/// observation (git itself said the directory is in no repository); everything
/// else is a failure to determine.
enum TopErr {
    OutsideAnyRepo,
    Other(String),
}

impl std::fmt::Display for TopErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TopErr::OutsideAnyRepo => f.write_str("not inside any git repository"),
            TopErr::Other(s) => f.write_str(s),
        }
    }
}

/// Environment variables that steer git's repository discovery. They are
/// inherited from the hook process and must not influence a child that asks
/// "which repository contains this directory": e.g. `GIT_CEILING_DIRECTORIES`
/// makes an in-repo directory look outside every repository.
pub(crate) const GIT_DISCOVERY_ENV: [&str; 9] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_DISCOVERY_ACROSS_FILESYSTEM",
    "GIT_CEILING_DIRECTORIES",
    "GIT_NAMESPACE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
];

/// Remove every [`GIT_DISCOVERY_ENV`] variable from `cmd`'s environment.
pub(crate) fn clear_git_discovery_env(cmd: &mut Command) -> &mut Command {
    for k in GIT_DISCOVERY_ENV {
        cmd.env_remove(k);
    }
    cmd
}

fn toplevel(dir: &Path) -> Result<PathBuf, TopErr> {
    let mut cmd = Command::new("git");
    clear_git_discovery_env(&mut cmd);
    let out = cmd
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map_err(|e| TopErr::Other(format!("cannot run git in {}: {e}", dir.display())))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        // Positively outside: exit 128 AND the full phrase. A corrupt gitfile
        // says "not a git repository" without the parenthetical, and stays Other.
        if out.status.code() == Some(128)
            && stderr.contains("not a git repository (or any of the parent directories)")
        {
            return Err(TopErr::OutsideAnyRepo);
        }
        return Err(TopErr::Other(format!(
            "git rev-parse --show-toplevel failed in {} (exit {:?}): {}",
            dir.display(),
            out.status.code(),
            stderr.trim()
        )));
    }
    let s = String::from_utf8_lossy(&out.stdout);
    let s = s.trim();
    if s.is_empty() {
        return Err(TopErr::Other(format!(
            "git printed no toplevel for {}",
            dir.display()
        )));
    }
    std::fs::canonicalize(s)
        .map_err(|e| TopErr::Other(format!("cannot canonicalize toplevel {s}: {e}")))
}

/// Resolve the roots for a hook at `cwd` with the session transcript at
/// `transcript_path`. An empty `transcript_path` (manual CLI run) records no
/// writes: the only root is `cwd`.
pub fn resolve(cwd: &Path, transcript_path: &str) -> Determination<SessionRoots> {
    let cwd_c = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    let mut roots = vec![cwd_c];
    if transcript_path.is_empty() {
        return Determination::known(SessionRoots {
            roots,
            wrote_any: false,
        });
    }
    // A transcript file that does not exist records no writes: there is nothing
    // to resolve roots from, and every caller's per-root attribution is then
    // itself undetermined and judges the whole checkout (the restrictive side).
    // Any other read problem (permissions, unparsable content) is Undetermined
    // below and blocks.
    if matches!(
        std::fs::metadata(transcript_path),
        Err(ref e) if e.kind() == std::io::ErrorKind::NotFound
    ) {
        return Determination::known(SessionRoots {
            roots,
            wrote_any: false,
        });
    }
    let writes = match crate::attribution::session_written_paths(transcript_path) {
        Determination::Known(w) => w,
        Determination::Undetermined(why) => {
            return Determination::undetermined(format!(
                "session roots: cannot read the session's writes from the transcript: {}",
                why.as_str()
            ))
        }
    };
    let wrote_any = !writes.edited.is_empty() || !writes.bash.is_empty();
    for p in &writes.edited {
        let parent = match p.parent() {
            Some(d) if d.is_dir() => d,
            _ => {
                return Determination::undetermined(format!(
                    "session roots: the session wrote {} but its directory does not exist, \
                     so its git root cannot be determined",
                    p.display()
                ))
            }
        };
        match toplevel(parent) {
            Ok(t) => {
                if !roots.contains(&t) {
                    roots.push(t);
                }
            }
            // Positively outside every repo (absolute path, existing dir, git
            // exit 128 with the full message): no checkout to judge, so excluded.
            Err(TopErr::OutsideAnyRepo) if p.is_absolute() => {}
            Err(e) => {
                return Determination::undetermined(format!(
                    "session roots: the session wrote {} but its git root cannot be \
                     determined: {e}",
                    p.display()
                ))
            }
        }
    }
    for p in &writes.bash {
        let dir = if p.is_dir() {
            Some(p.as_path())
        } else {
            p.parent()
        };
        let Some(dir) = dir else { continue };
        if let Ok(t) = toplevel(dir) {
            if !roots.contains(&t) {
                roots.push(t);
            }
        }
    }
    Determination::known(SessionRoots { roots, wrote_any })
}
