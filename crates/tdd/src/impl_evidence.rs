//! Write evidence for the Stop hook's empty-scan rule ("the session wrote files
//! but no implementation change was evaluated in any root"). Split out of
//! `gate.rs` to keep that file under its line cap (scripts/check-file-growth).

use std::path::{Path, PathBuf};

use harness_core::verdict::Determination;

use crate::config::Config;
use crate::gate::build_globset;

/// Is this path an implementation file by `cfg`'s own classification (an
/// impl glob match that is not a test path)? The same predicate
/// `gate::classify_with_removed` counts added lines with.
pub fn is_impl_path(cfg: &Config, path: &str) -> bool {
    let impl_set = build_globset(&cfg.impl_globs);
    let test_set = build_globset(&cfg.test_path_globs);
    impl_set.is_match(path) && !test_set.is_match(path)
}

/// Did the session write something tdd itself would treat as an
/// implementation change? This is the write evidence the Stop hook's
/// empty-scan rule counts (user ruling 2026-11-05: an integration /
/// bookkeeping session — `git merge`, `backlog done` + a commit of
/// `.backlog/*.toml`, `scripts/rollout-plugins.sh`, writes outside every repo —
/// is not blocked by it).
///
/// Counted: every Edit/Write/MultiEdit/NotebookEdit target and every Bash
/// write candidate (`harness_core::attribution::SessionWrites`) that is NOT an
/// existing directory (a `cd` target) and IS an implementation file
/// ([`is_impl_path`]) under the config of the deepest root containing it,
/// matched on its root-relative path. A path under no root is matched on its
/// full path with `fallback` (the hook cwd's config), so an implementation
/// file outside every judged root still counts.
///
/// Not distinguished (stated, not hidden): the executed program word of a Bash
/// statement is not separated from its arguments — an executed `./x.py`
/// counts as an implementation write. That over-counts, i.e. errs toward
/// blocking.
///
/// An unreadable transcript (`session_written_paths` undetermined) is `true`:
/// unable to tell is not "only bookkeeping" (CLAUDE.md §3).
pub fn session_wrote_impl(
    roots: &[(PathBuf, Config)],
    fallback: &Config,
    transcript_path: &str,
) -> bool {
    let writes = match harness_core::attribution::session_written_paths(transcript_path) {
        Determination::Known(w) => w,
        Determination::Undetermined(_) => return true,
    };
    let edited = writes.edited.iter();
    let bash = writes.bash.iter().filter(|p| !p.is_dir());
    edited
        .chain(bash)
        .any(|p| wrote_impl_path(roots, fallback, p))
}

fn wrote_impl_path(roots: &[(PathBuf, Config)], fallback: &Config, p: &Path) -> bool {
    // Deepest containing root: a linked worktree usually lives inside the
    // primary checkout (`.claude/worktrees/...`).
    let owner = roots
        .iter()
        .filter_map(|(r, c)| p.strip_prefix(r).ok().map(|rel| (r, c, rel)))
        .max_by_key(|(r, _, _)| r.components().count());
    match owner {
        Some((_, cfg, rel)) => is_impl_path(cfg, &rel.to_string_lossy()),
        None => is_impl_path(fallback, &p.to_string_lossy()),
    }
}
