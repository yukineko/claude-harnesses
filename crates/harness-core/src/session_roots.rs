//! The set of checkouts a Stop gate must judge.
//!
//! A gate that uses only the hook `cwd` judges the wrong tree when the session
//! worked in a linked git worktree while the hook ran in the primary tree: it
//! sees a clean tree and allows. The roots here are the hook cwd plus the git
//! toplevel of every file the session transcript records as written.
//!
//! Fail-closed (CLAUDE.md section 3): an unreadable transcript, a written file
//! whose directory is gone, or one that is not inside a git work tree is
//! `Undetermined` naming the path. It is never dropped from the set. The one
//! exception is a write outside git that lies in this session's own Claude
//! Code scratchpad (see `in_own_scratchpad`): that is not a root and not an error.
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

fn toplevel(dir: &Path) -> Result<PathBuf, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map_err(|e| format!("cannot run git in {}: {e}", dir.display()))?;
    if !out.status.success() {
        return Err(format!(
            "git rev-parse --show-toplevel failed in {} (exit {:?}): {}",
            dir.display(),
            out.status.code(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let s = String::from_utf8_lossy(&out.stdout);
    let s = s.trim();
    if s.is_empty() {
        return Err(format!("git printed no toplevel for {}", dir.display()));
    }
    std::fs::canonicalize(s).map_err(|e| format!("cannot canonicalize toplevel {s}: {e}"))
}

/// Whether `dir` lies inside THIS session's Claude Code scratchpad:
/// canonically `…/claude-<digits>/<slug>/<SID>/scratchpad[/…]`, where `<SID>`
/// is the transcript's file stem. Claude Code tells the model to put temporary
/// files there, and nothing a gate checks lives in it. Another session's
/// scratchpad, or any other directory outside git, does not match and stays
/// undetermined. A directory that cannot be canonicalized does not match.
fn in_own_scratchpad(dir: &Path, transcript_path: &str) -> bool {
    let Some(sid) = Path::new(transcript_path)
        .file_stem()
        .and_then(|s| s.to_str())
    else {
        return false;
    };
    let Ok(canon) = std::fs::canonicalize(dir) else {
        return false;
    };
    let comps: Vec<&std::ffi::OsStr> = canon.iter().collect();
    comps.windows(4).any(|w| {
        let uid_ok = w[0]
            .to_str()
            .and_then(|s| s.strip_prefix("claude-"))
            .is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()));
        uid_ok && w[2] == sid && w[3] == "scratchpad"
    })
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
            Err(_) if in_own_scratchpad(parent, transcript_path) => {}
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
