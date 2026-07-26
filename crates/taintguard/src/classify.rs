//! Path-trust classifier: is a `Read` target inside the project root (trusted)
//! or outside it (untrusted — `/tmp`, the home dir, another project, or a
//! `..`-escaping relative path)?
//!
//! There is no existing trusted/untrusted path notion in this repo; this is
//! new logic. Kept deliberately simple and conservative: anything that cannot
//! be positively resolved inside the root is `Untrusted` or `Indeterminate`,
//! never `Trusted` — the caller (`hooks::mark`) treats both non-`Trusted`
//! answers the same way (mark the session tainted), so the only distinction
//! that matters operationally is "inside the root" vs "everything else".

use std::path::{Component, Path, PathBuf};

/// Trust verdict for a single path, relative to a project root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trust {
    /// Resolves to somewhere inside `root`.
    Trusted,
    /// Resolves to somewhere outside `root`.
    Untrusted,
    /// Could not be resolved at all (empty target, or `root` itself could not
    /// be canonicalized). Callers must treat this the same as `Untrusted`
    /// (fail-closed) — see the module docs.
    Indeterminate,
}

/// Collapse `.`/`..` components of `path` **lexically** (no filesystem access,
/// no symlink resolution) — used as the fallback when `path` does not exist
/// yet (so `Path::canonicalize` would fail) but still needs `..`-escape
/// detection. A leading `..` past the root simply has nothing left to pop,
/// which is fine: the result still won't `starts_with` the root.
fn normalize_lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Classify `target` (a `Read` tool's `file_path`, as given — may be relative
/// or absolute) against `root` (the hook's `cwd`).
///
/// Resolution: `target` is joined onto `root` when relative, then resolved
/// with `canonicalize()` (follows symlinks — a symlink inside the root
/// pointing outside it correctly classifies as `Untrusted`) when the path
/// exists on disk; when it does not (a `Read` of a not-yet-created file, or a
/// test fixture that never touches disk), falls back to the lexical
/// normalization above. `root` itself is always canonicalized; if that fails,
/// there is no trustworthy boundary to compare against, so the answer is
/// `Indeterminate`.
pub fn classify(root: &Path, target: &str) -> Trust {
    let trimmed = target.trim();
    if trimmed.is_empty() {
        return Trust::Indeterminate;
    }
    // A literal, unexpanded leading `~` (`~/secret`, `~otheruser/x`) is never
    // what a real `Read` tool_input carries (Claude Code always resolves it
    // to an absolute path first) — so seeing one here means the shape is
    // unverified from this function's own resolution. Naively joining it onto
    // `root` would create a literal `<root>/~/secret` path that lexically
    // starts_with `root` and misclassifies as `Trusted`. Defensive: anything
    // we cannot confidently resolve to inside the root is `Untrusted`, not
    // `Trusted` (see FIX #4 in the crate's issue history).
    if trimmed.starts_with('~') {
        return Trust::Untrusted;
    }
    let Ok(root_canonical) = root.canonicalize() else {
        return Trust::Indeterminate;
    };
    let target_path = Path::new(target);
    let joined = if target_path.is_absolute() {
        target_path.to_path_buf()
    } else {
        // Join onto the CANONICAL root, not the raw one: on macOS a tempdir
        // path (`/var/folders/...`) is itself a symlink to
        // `/private/var/folders/...`, so joining onto the raw root and only
        // canonicalizing the root separately would compare two differently-
        // rooted paths and misclassify every not-yet-existing in-repo target
        // as escaping (see `harness_core::store::context_state_dir`'s own
        // canonicalize-before-join discipline for the same reason).
        root_canonical.join(target_path)
    };
    let resolved = joined
        .canonicalize()
        .unwrap_or_else(|_| normalize_lexical(&joined));
    if resolved.starts_with(&root_canonical) {
        Trust::Trusted
    } else {
        Trust::Untrusted
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix(&format!("taintguard-classify-{name}-"))
            .tempdir()
            .expect("tempdir")
    }

    #[test]
    fn in_repo_relative_path_is_trusted() {
        let root = temp_root("relative");
        std::fs::write(root.path().join("a.rs"), "x").unwrap();
        assert_eq!(classify(root.path(), "a.rs"), Trust::Trusted);
    }

    #[test]
    fn in_repo_absolute_path_is_trusted() {
        let root = temp_root("absolute");
        let sub = root.path().join("crates").join("x");
        std::fs::create_dir_all(&sub).unwrap();
        let f = sub.join("main.rs");
        std::fs::write(&f, "x").unwrap();
        assert_eq!(classify(root.path(), &f.to_string_lossy()), Trust::Trusted);
    }

    #[test]
    fn sibling_dir_outside_root_is_untrusted() {
        let root = temp_root("sibling-root");
        let inside = root.path().join("inside");
        std::fs::create_dir_all(&inside).unwrap();
        let outside_base = tempfile::Builder::new()
            .prefix("taintguard-classify-sibling-outside-")
            .tempdir()
            .unwrap();
        let outside = outside_base.path().join("secret.txt");
        std::fs::write(&outside, "s").unwrap();
        assert_eq!(
            classify(&inside, &outside.to_string_lossy()),
            Trust::Untrusted
        );
    }

    #[test]
    fn dotdot_escape_is_untrusted() {
        let root = temp_root("dotdot");
        let inside = root.path().join("inside");
        std::fs::create_dir_all(&inside).unwrap();
        // Escapes `inside` back up to a sibling of the project root — must not
        // be trusted just because the string starts inside the tree.
        assert_eq!(classify(&inside, "../../etc/passwd"), Trust::Untrusted);
    }

    #[test]
    fn dotdot_that_resolves_back_inside_is_trusted() {
        let root = temp_root("dotdot-back-inside");
        let sub = root.path().join("a").join("b");
        std::fs::create_dir_all(&sub).unwrap();
        // a/b/../c lexically resolves to a/c, still inside root.
        assert_eq!(classify(root.path(), "a/b/../c.rs"), Trust::Trusted);
    }

    #[test]
    fn empty_target_is_indeterminate() {
        let root = temp_root("empty");
        assert_eq!(classify(root.path(), ""), Trust::Indeterminate);
        assert_eq!(classify(root.path(), "   "), Trust::Indeterminate);
    }

    #[test]
    fn unresolvable_root_is_indeterminate() {
        let missing_root = Path::new("/definitely/does/not/exist/taintguard-root");
        assert_eq!(classify(missing_root, "a.rs"), Trust::Indeterminate);
    }

    #[test]
    fn nonexistent_target_still_classifies_lexically() {
        let root = temp_root("nonexistent-target");
        // The file does not exist (a `Read` of a not-yet-created path), so
        // canonicalize fails; the lexical fallback must still classify it as
        // inside the root.
        assert_eq!(classify(root.path(), "not-yet-created.rs"), Trust::Trusted);
        assert_eq!(
            classify(root.path(), "../outside-not-yet-created.rs"),
            Trust::Untrusted
        );
    }

    #[test]
    fn tmp_style_absolute_path_is_untrusted() {
        let root = temp_root("tmp-style");
        assert_eq!(
            classify(root.path(), "/tmp/some-scratch-file"),
            Trust::Untrusted
        );
    }

    /// FIX #4: an unexpanded leading `~` must never be trusted just because
    /// joining it onto the root lexically starts with the root.
    #[test]
    fn unexpanded_tilde_path_is_untrusted() {
        let root = temp_root("tilde");
        assert_eq!(classify(root.path(), "~/secret"), Trust::Untrusted);
        assert_eq!(classify(root.path(), "~"), Trust::Untrusted);
        assert_eq!(classify(root.path(), "~otheruser/x"), Trust::Untrusted);
    }
}
