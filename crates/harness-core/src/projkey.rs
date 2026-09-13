//! Per-project addressing for run-state files — the SINGLE source of truth.
//!
//! A project key is `<sanitized-basename>-<fnv1a32-hex-of-canonical-root>`: the
//! basename keeps it readable, the hash keeps two same-named repos from
//! colliding. FNV-1a is dependency-free and stable across runs.
//!
//! This lives in harness-core because more than one plugin must derive the SAME
//! key for the same repo: condukt writes its run-state under `project_key(root)`
//! and autoflow reads that very directory. When each crate kept its own private
//! copy, a change in one would silently send the other to a different directory
//! (losing state). Both now call this; they cannot drift.
//!
//! NOTE: this is intentionally distinct from `store::project_key`, which keys the
//! per-cwd note store with a different (alnum-only, non-canonicalized) scheme.

use crate::verdict::Determination;
use std::path::{Component, Path, PathBuf};

/// FNV-1a 32-bit hash. Re-exported from `crate::hash` — the single FNV-1a
/// implementation — and kept here as the historic public path used to derive
/// project keys.
pub use crate::hash::fnv1a32;

/// Stable per-project key derived from the canonical repo root.
pub fn project_key(root: &Path) -> String {
    let canon = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let full = canon.to_string_lossy();
    // `"root"` is only the human-readable *prefix* for a path with no basename
    // (e.g. "/"). It is NOT a collision vector: the key's uniqueness comes
    // entirely from the `fnv1a32(&full)` suffix below (the full canonical path),
    // so two distinct rootless paths still get distinct keys. See the
    // `rootless_paths_do_not_collide` test.
    let base = canon
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "root".into());
    let sani: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    format!("{}-{:08x}", sani, fnv1a32(&full))
}

/// Nearest ancestor containing `.git`; falls back to `cwd` if none.
pub fn repo_root(cwd: &Path) -> PathBuf {
    let mut cur = cwd.to_path_buf();
    loop {
        if cur.join(".git").exists() {
            return cur;
        }
        if !cur.pop() {
            break;
        }
    }
    cwd.to_path_buf()
}

/// The MAIN worktree root for `cwd` — the identity that is stable across a
/// repo's linked worktrees. Distinct from [`repo_root`], which stops at the
/// first ancestor with a `.git` entry and therefore returns the LINKED
/// WORKTREE itself (there `.git` is a file, and `.exists()` is true).
///
/// # Why this is a second function instead of a fix to `repo_root`
///
/// [`repo_root`] addresses run-state, precedent and autoflow stores that are
/// ALREADY on disk under worktree-derived keys. Redefining it would relocate
/// every one of those stores at once and orphan the existing files. That wider
/// migration is tracked separately as backlog `43393ce2`; this function exists
/// so a store whose contract requires the repo-wide identity (condukt's
/// cross-session claim registry) can opt in WITHOUT moving the others.
///
/// # Resolution (pure filesystem — no subprocess, so no `git` dependency)
///
/// 1. `root = repo_root(cwd)`.
/// 2. `root/.git` is a DIRECTORY, or absent entirely ⇒ `Known(root)`: there is
///    no linked-worktree indirection to follow.
/// 3. `root/.git` is a FILE ⇒ read its `gitdir: <path>` line (absolute, or
///    relative to `root`). Anything else ⇒ `Undetermined`.
/// 4. `<gitdir>/commondir` ABSENT ⇒ this is a `--separate-git-dir` repo or a
///    submodule, NOT a linked worktree ⇒ `Known(root)`.
/// 5. Present ⇒ its contents (typically `../..`) resolve, relative to
///    `<gitdir>`, to the common git dir.
/// 6. The candidate main root is the common git dir's PARENT, and it is
///    VERIFIED before being returned (see below).
///
/// # The verification in step 6 is load-bearing
///
/// A worktree OF A SUBMODULE has a common dir of `<super>/.git/modules/<name>`,
/// whose parent `<super>/.git/modules` is not a worktree root at all. Returning
/// it unverified would be a fail-open: a bogus-but-`Known` identity reads
/// downstream as a successfully resolved project. So the candidate must carry a
/// `.git` entry that actually designates the same common dir, and when it does
/// not the answer is `Undetermined` (CLAUDE.md §3) — never a guess.
///
/// Every `Undetermined` reason is prefixed `main-worktree-root:` and carries a
/// distinct tag after it, so each arm is individually greppable.
pub fn main_worktree_root(cwd: &Path) -> Determination<PathBuf> {
    let root = repo_root(cwd);
    let dot_git = root.join(".git");

    match std::fs::metadata(&dot_git) {
        // A real `.git` directory: this IS the main worktree.
        Ok(m) if m.is_dir() => return Determination::known(root),
        // A `.git` FILE: a gitdir pointer. Follow it below.
        Ok(_) => {}
        // No `.git` at all — `repo_root` fell back to `cwd` because no ancestor
        // is a repo. There is no indirection to follow, and keying on `cwd` is
        // exactly what `repo_root` already resolved to.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Determination::known(root),
        Err(e) => {
            return Determination::undetermined(format!(
                "main-worktree-root: stat-failed: cannot stat {} ({e}); whether this \
                 is a linked worktree cannot be decided",
                dot_git.display()
            ))
        }
    }

    let gitfile = match crate::boundary::read_to_string(&dot_git) {
        Determination::Known(Some(text)) => text,
        // Raced away between the stat above and this read: we saw a file, then
        // it was gone. That is "cannot determine", not "plain repo".
        Determination::Known(None) => {
            return Determination::undetermined(format!(
                "main-worktree-root: gitfile-vanished: {} was a file when stat'd and \
                 absent when read",
                dot_git.display()
            ))
        }
        // Forward the boundary's reason verbatim (it already names the file);
        // re-minting here would double-record the same undetermined event.
        Determination::Undetermined(why) => return Determination::Undetermined(why),
    };

    let gitdir = match gitdir_target(&gitfile) {
        Some(p) => resolve_against(&root, Path::new(&p)),
        None => {
            return Determination::undetermined(format!(
                "main-worktree-root: gitfile-unparseable: {} is a .git FILE with no \
                 `gitdir: <path>` line, so the git dir it designates is unknown",
                dot_git.display()
            ))
        }
    };

    let commondir_file = gitdir.join("commondir");
    let commondir_text = match crate::boundary::read_to_string(&commondir_file) {
        Determination::Known(Some(text)) => text,
        // No `commondir` ⇒ the gitfile points at a standalone git dir
        // (`--separate-git-dir`, or a submodule's git dir), not at a linked
        // worktree's admin dir. `root` is its own main worktree root.
        Determination::Known(None) => return Determination::known(root),
        Determination::Undetermined(why) => return Determination::Undetermined(why),
    };
    let commondir_line = commondir_text.trim();
    if commondir_line.is_empty() {
        return Determination::undetermined(format!(
            "main-worktree-root: commondir-empty: {} exists but names no path, so the \
             repo's common git dir is unknown",
            commondir_file.display()
        ));
    }
    let common_dir = resolve_against(&gitdir, Path::new(commondir_line));

    let candidate = match common_dir.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => {
            return Determination::undetermined(format!(
                "main-worktree-root: commondir-has-no-parent: the common git dir {} \
                 has no parent directory that could be a worktree root",
                common_dir.display()
            ))
        }
    };

    // ── step 6: VERIFY the candidate actually owns `common_dir` ──────────────
    let candidate_git = candidate.join(".git");
    match std::fs::metadata(&candidate_git) {
        Ok(m) if m.is_dir() => {
            if !same_path(&candidate_git, &common_dir) {
                return Determination::undetermined(format!(
                    "main-worktree-root: candidate-git-mismatch: {} has a .git directory \
                     that is not the common git dir {} named by {}",
                    candidate.display(),
                    common_dir.display(),
                    commondir_file.display()
                ));
            }
        }
        Ok(_) => {
            // The candidate's own `.git` is itself a gitfile (the main worktree
            // of a submodule looks like this). It must point INTO `common_dir`.
            let text = match crate::boundary::read_to_string(&candidate_git) {
                Determination::Known(Some(t)) => t,
                Determination::Known(None) => {
                    return Determination::undetermined(format!(
                        "main-worktree-root: candidate-gitfile-vanished: {} was a file \
                         when stat'd and absent when read",
                        candidate_git.display()
                    ))
                }
                Determination::Undetermined(why) => return Determination::Undetermined(why),
            };
            let target = match gitdir_target(&text) {
                Some(p) => resolve_against(&candidate, Path::new(&p)),
                None => {
                    return Determination::undetermined(format!(
                        "main-worktree-root: candidate-gitfile-unparseable: {} has no \
                         `gitdir: <path>` line, so it cannot be shown to designate {}",
                        candidate_git.display(),
                        common_dir.display()
                    ))
                }
            };
            if !path_starts_with(&target, &common_dir) {
                return Determination::undetermined(format!(
                    "main-worktree-root: candidate-gitfile-mismatch: {} designates {}, \
                     which is not inside the common git dir {}",
                    candidate_git.display(),
                    target.display(),
                    common_dir.display()
                ));
            }
        }
        // The single most important arm: a worktree OF A SUBMODULE lands here,
        // because `<super>/.git/modules` has no `.git` of its own. Returning the
        // candidate anyway would hand back a directory that is not a worktree
        // root at all.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Determination::undetermined(format!(
                "main-worktree-root: candidate-has-no-git: {} (parent of the common git \
                 dir {}) has no .git entry, so it is not a worktree root",
                candidate.display(),
                common_dir.display()
            ))
        }
        Err(e) => {
            return Determination::undetermined(format!(
                "main-worktree-root: candidate-stat-failed: cannot stat {} ({e}), so the \
                 candidate main worktree cannot be verified",
                candidate_git.display()
            ))
        }
    }

    Determination::known(candidate)
}

/// The `<path>` of the first `gitdir: <path>` line in a `.git` FILE, or `None`
/// when there is no such line (or it names nothing).
fn gitdir_target(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix("gitdir:"))
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(str::to_string)
}

/// `p` resolved against `base` when relative, then normalized lexically.
fn resolve_against(base: &Path, p: &Path) -> PathBuf {
    if p.is_absolute() {
        normalize_lexically(p)
    } else {
        normalize_lexically(&base.join(p))
    }
}

/// Resolve `.`/`..` segments without touching the filesystem.
///
/// Deliberately NOT `canonicalize`: git's `commondir` is written relative to a
/// path that may not exist by the time this runs, and `canonicalize` fails on a
/// missing path — a resolution that fails for a reason unrelated to the question
/// being asked. A `..` with nothing above it to pop is KEPT (there is no
/// filesystem fact available to drop it against).
fn normalize_lexically(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in p.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(out.components().next_back(), Some(Component::Normal(_))) {
                    out.pop();
                } else {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Canonical form when the path resolves, the lexical form when it does not.
///
/// The fallback is not a permissive default: both sides of every comparison
/// below go through it, and a comparison that then disagrees resolves to
/// `Undetermined` — the restrictive side.
fn canon_or_lexical(p: &Path) -> PathBuf {
    p.canonicalize().unwrap_or_else(|_| normalize_lexically(p))
}

fn same_path(a: &Path, b: &Path) -> bool {
    canon_or_lexical(a) == canon_or_lexical(b)
}

fn path_starts_with(child: &Path, ancestor: &Path) -> bool {
    canon_or_lexical(child).starts_with(canon_or_lexical(ancestor))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_is_stable_and_sanitized() {
        let p = PathBuf::from("/tmp/My Repo");
        let k1 = project_key(&p);
        let k2 = project_key(&p);
        assert_eq!(k1, k2);
        // basename sanitized (space -> '-'), hash suffix present.
        assert!(k1.starts_with("My-Repo-"));
        assert_eq!(k1.len(), "My-Repo-".len() + 8);
    }

    #[test]
    fn distinct_paths_get_distinct_hashes() {
        let a = project_key(&PathBuf::from("/tmp/proj"));
        let b = project_key(&PathBuf::from("/var/proj"));
        assert_ne!(a, b);
    }

    #[test]
    fn rootless_paths_do_not_collide() {
        // Paths whose last component is `..` have no basename (file_name() ==
        // None) and so both take the "root" readable prefix. Using non-existent
        // paths makes canonicalize fall back to the raw path, so the full-path
        // hash differs — proving the fallback prefix is never a collision vector.
        let a = project_key(Path::new("/no-such-aaa/.."));
        let b = project_key(Path::new("/no-such-bbb/.."));
        assert!(a.starts_with("root-"), "got {a}");
        assert!(b.starts_with("root-"), "got {b}");
        assert_ne!(a, b, "distinct rootless paths must get distinct keys");
    }

    #[test]
    fn fnv_known_vector() {
        // FNV-1a 32-bit of empty string is the offset basis.
        assert_eq!(fnv1a32(""), 0x811c_9dc5);
        // A second fixed vector pins the multiply step so the algorithm can't
        // silently change (which would relocate every project's state dir).
        assert_eq!(fnv1a32("a"), 0xe40c_292c);
    }

    #[test]
    fn key_format_is_basename_dash_hash() {
        // Guards the on-disk layout: <sanitized-basename>-<8 hex>. A non-existent
        // path can't be canonicalized, so the input path is used as-is.
        let p = PathBuf::from("/tmp/proj_x");
        let expected = format!("proj_x-{:08x}", fnv1a32(&p.to_string_lossy()));
        assert_eq!(project_key(&p), expected);
    }
}
