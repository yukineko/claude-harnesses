use std::path::{Path, PathBuf};

use harness_core::config::home;
use harness_core::projkey::repo_root;
use harness_core::verdict::Determination;

// RETIRED 2026-08-20 (user instruction): `BacklogItem` and `find_open` were
// here. They read `backlog list --status pending --json` for the two nudges that
// asked the operator to work the queue — the Stop hook's "/backlog を実行して
// ください" arm and the SessionStart "/flow で開始しますか？" proposal. With both
// gone nothing consumes the queue in this crate, and a reader with no consumer is
// an invitation to re-wire it, so it goes with them. Its three-valued contract
// (`Known(vec![])` only for an ASKED-and-empty queue, `Undetermined` for every
// way of failing to ask) is preserved where it is still load-bearing:
// `condukt::find_pending` and `find_backlog_binary` below.

/// Locate the `backlog` binary: PATH first, then the plugin cache.
///
/// `Known(None)` is the *observation* that backlog is not installed (no plugin
/// cache directory at all). An enumerable-but-failing cache directory — the
/// read denied, an entry unreadable, a candidate whose existence cannot be
/// tested — is `Undetermined`: collapsing it into the same `None` is what let a
/// merely unreadable directory read as "backlog is not installed" and start an
/// unattended auto-loop next to a live driver (audit §4.5, the one permissive-A
/// path).
pub(crate) fn find_backlog_binary() -> Determination<Option<PathBuf>> {
    if std::process::Command::new("backlog")
        .arg("--version")
        .output()
        .is_ok()
    {
        return Determination::Known(Some(PathBuf::from("backlog")));
    }

    // ~/.claude/plugins/cache/yukineko/backlog/<version>/bin/backlog
    let base = home()
        .join(".claude")
        .join("plugins")
        .join("cache")
        .join("yukineko")
        .join("backlog");

    let dir = match std::fs::read_dir(&base) {
        Ok(d) => d,
        // No cache dir ⇒ backlog was never installed here. An observation.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Determination::Known(None),
        // Anything else (permission denied, IO error) ⇒ we did not get to look.
        Err(e) => {
            return Determination::undetermined(format!(
                "could not enumerate {}: {e}",
                base.display()
            ))
        }
    };

    let mut candidates: Vec<PathBuf> = Vec::new();
    for entry in dir {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                return Determination::undetermined(format!(
                    "could not read an entry of {}: {e}",
                    base.display()
                ))
            }
        };
        let candidate = entry.path().join("bin").join("backlog");
        // `exists()` folds "not there" and "cannot tell" into one `false`;
        // `try_exists()` keeps them apart.
        match candidate.try_exists() {
            Ok(true) => candidates.push(candidate),
            Ok(false) => {}
            Err(e) => {
                return Determination::undetermined(format!(
                    "could not test {}: {e}",
                    candidate.display()
                ))
            }
        }
    }

    candidates.sort();
    Determination::Known(candidates.pop())
}

/// The repo root as a stable, *unique* project filter for `backlog list`.
///
/// The previous `repo_basename` returned only the directory name, with a
/// constant `"unknown"` fallback for a rootless path. Both are predictable
/// collisions: every repo sharing a basename (e.g. two checkouts named `app`),
/// and every non-git directory (all → `"unknown"`), addressed one another's
/// backlog state. We instead use the canonical absolute path, which is unique
/// per repo and matches how tasks are stored (`backlog add --project "$PWD"`,
/// a full path) under `project_matches`'s exact/prefix rule. Canonicalize
/// failure falls back to the raw absolute path — still unique, never a constant.
pub(crate) fn repo_project_path(cwd: &Path) -> String {
    let root = repo_root(cwd);
    root.canonicalize()
        .unwrap_or(root)
        .to_string_lossy()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Two non-git directories that share a basename used to both collapse to the
    // same `--project` value (the basename, or the constant "unknown"), so one
    // repo's autoflow saw the other's backlog. The path-based key keeps them
    // distinct. (These paths don't exist, so canonicalize falls back to the raw
    // path — exactly the rootless/non-git case the old fallback mishandled.)
    #[test]
    fn same_basename_distinct_paths_do_not_collide() {
        let a = repo_project_path(Path::new("/tmp/aaa/app"));
        let b = repo_project_path(Path::new("/var/bbb/app"));
        assert_ne!(a, b, "same-basename repos must get distinct project keys");
        assert!(!a.is_empty() && !b.is_empty());
        // Never the old constant fallback.
        assert_ne!(a, "unknown");
        assert_ne!(b, "unknown");
    }

    // The key matches how tasks are stored: `backlog add --project "$PWD"` uses a
    // full path, and backlog's `project_matches` accepts an exact or prefix hit.
    #[test]
    fn key_is_the_full_path_for_a_non_git_dir() {
        // A path with no .git ancestor → repo_root returns the path itself.
        let p = Path::new("/tmp/some/non-git-dir");
        assert_eq!(repo_project_path(p), "/tmp/some/non-git-dir");
    }

    // RETIRED 2026-08-20 with `BacklogItem` itself: this pinned the
    // producer/consumer contract (`backlog list --json` keys the human title
    // `title`, which the consumer had to map onto `text`). autoflow no longer
    // consumes that JSON at all, so the contract it pinned has no consumer here.
    // Deleted because the behaviour is gone, NOT because it went red — the
    // distinction CLAUDE.md 4 turns on. The same shape is still pinned where it
    // is still consumed: backlog's own integration test, and `flow`'s reader
    // before it was retired.
}
