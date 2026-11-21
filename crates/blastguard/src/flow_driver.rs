//! Is this hook call made by a session registered as a live `/flow` driver?
//!
//! # The ruling
//!
//! User ruling 2026-11-21 (verbatim: 「非破壊はAllow、他はDeny」), refined the
//! same day (「またaskされてたけど、これもallowに」): in a session that is a
//! registered `/flow` driver, blastguard never answers `ask`. A `/flow` driver
//! runs unattended; a permission prompt there is a stall, not a question. So
//! every verdict that would be `Ask` becomes `Allow` when it is statically safe
//! and `Deny` otherwise (the hook binary does the mapping; see `main.rs`
//! `emit`). The statically-safe program runs — a script in the session
//! scratchpad, a binary under the session's own worktree `target/` — are
//! judged in the Table B.5 layer through
//! [`crate::scope::SafeRoots::with_driver_programs`].
//!
//! # The registry read (mirrors `crates/backlog/src/driver.rs`)
//!
//! blastguard cannot link `backlog`, so this is a minimal re-implementation of
//! the READ side of `backlog driver register --session-id S --project P`:
//!
//! * `<backlog base>/drivers/<project slug>/<fnv1a64(S) as %016x>.driver`,
//!   where `<backlog base>` is `harness_core::config::base_dir("backlog")`
//!   (`~/.backlog`), passed in by the caller;
//! * `<project slug>` = `%016x` of `fnv1a64(<canonical project>)`, and the
//!   canonical project of a cwd is backlog's `store::canonical_project_id`: the
//!   nearest ancestor holding a `.git` entry; a `.git` DIRECTORY → that root,
//!   canonicalised; a `.git` FILE (linked worktree) → its main working tree,
//!   verified the same way backlog verifies it, canonicalised; no ancestor →
//!   the cwd itself, canonicalised;
//! * the file is JSON with at least `session_id` and `heartbeat_at`; the
//!   registration is live while `now - heartbeat_at <= 1800` (backlog's
//!   `LOCK_STALE_TTL_SECS`).
//!
//! The fixture in `tests/backlog_20261121_flow_driver_session_never_asks.rs`
//! writes the registration byte-for-byte the way `backlog` does and pins this
//! read against it.
//!
//! # Cannot determine is not "driver"
//!
//! Driver mode turns some `Ask`s into `Allow`. So an unreadable registry, an
//! unparseable record or a project identity that cannot be resolved is
//! [`Determination::Undetermined`], and the caller treats it exactly like "not
//! a driver": the verdict is left unchanged (CLAUDE.md §3 — it must never
//! become an `Allow`). A record file that does not exist is an observation
//! (`Known(false)`), as are a stale heartbeat and a record whose `session_id`
//! is not this session's.

use std::path::{Path, PathBuf};

use harness_core::verdict::Determination;

/// backlog's `LOCK_STALE_TTL_SECS` (`crates/backlog/src/lock.rs`), which the
/// driver registry uses for staleness.
pub const DRIVER_STALE_TTL_SECS: i64 = 1800;

/// `%016x` of the shared FNV-1a, as backlog spells both slugs.
fn slug(s: &str) -> String {
    format!("{:016x}", harness_core::hash::fnv1a64(s.as_bytes()))
}

/// The nearest ancestor of `start` (inclusive) holding a `.git` entry —
/// backlog's `config::repo_root` (it uses `exists()`, which follows symlinks;
/// so does this).
pub fn repo_root(start: &Path) -> Option<PathBuf> {
    let mut dir = start.to_path_buf();
    loop {
        if dir.join(".git").exists() {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// backlog's `store::main_worktree_of`: the main working tree of the linked
/// worktree `root`, verified (the candidate's own `.git` must canonicalise to
/// the common git dir), or `None`.
fn main_worktree_of(root: &Path) -> Option<PathBuf> {
    let dot_git = root.join(".git");
    if !std::fs::symlink_metadata(&dot_git).ok()?.is_file() {
        return None;
    }
    // Absent or unreadable are both "could not resolve" here (`None`), which
    // `canonical_project` reports as `Undetermined`.
    let Determination::Known(Some(text)) = harness_core::boundary::read_to_string(&dot_git) else {
        return None;
    };
    let target = text
        .lines()
        .find_map(|l| l.trim().strip_prefix("gitdir:"))?
        .trim();
    if target.is_empty() {
        return None;
    }
    let target = Path::new(target);
    let git_dir = if target.is_absolute() {
        target.to_path_buf()
    } else {
        root.join(target)
    };
    let worktrees = git_dir.parent()?;
    if worktrees.file_name()? != "worktrees" {
        return None;
    }
    let common = worktrees.parent()?;
    let candidate = common.parent()?;
    let common_key = std::fs::canonicalize(common).ok()?;
    let candidate_key = std::fs::canonicalize(candidate.join(".git")).ok()?;
    if common_key != candidate_key {
        return None;
    }
    Some(candidate.to_path_buf())
}

/// backlog's `store::canonical_project_id` for `cwd`.
pub fn canonical_project(cwd: &Path) -> Determination<PathBuf> {
    let Some(root) = repo_root(cwd) else {
        return match cwd.canonicalize() {
            Ok(c) => Determination::known(c),
            Err(e) => Determination::undetermined(format!(
                "{} is outside any repo and could not be canonicalized: {e}",
                cwd.display()
            )),
        };
    };
    let dot_git = root.join(".git");
    match std::fs::symlink_metadata(&dot_git) {
        Ok(meta) if meta.is_file() => match main_worktree_of(&root) {
            Some(main) => match main.canonicalize() {
                Ok(c) => Determination::known(c),
                Err(e) => Determination::undetermined(format!(
                    "main working tree {} could not be canonicalized: {e}",
                    main.display()
                )),
            },
            None => Determination::undetermined(format!(
                "{} is a linked worktree whose main working tree could not be resolved",
                root.display()
            )),
        },
        Ok(_) => match root.canonicalize() {
            Ok(c) => Determination::known(c),
            Err(e) => Determination::undetermined(format!(
                "repo root {} could not be canonicalized: {e}",
                root.display()
            )),
        },
        Err(e) => Determination::undetermined(format!("could not stat {}: {e}", dot_git.display())),
    }
}

/// The registration file `backlog driver register` writes for `session_id`
/// in the project of `cwd`, under `backlog_base` (`~/.backlog`).
pub fn driver_file(backlog_base: &Path, cwd: &Path, session_id: &str) -> Determination<PathBuf> {
    match canonical_project(cwd) {
        Determination::Known(project) => {
            let project = project.to_string_lossy().into_owned();
            Determination::known(
                backlog_base
                    .join("drivers")
                    .join(slug(&project))
                    .join(format!("{}.driver", slug(session_id))),
            )
        }
        Determination::Undetermined(why) => Determination::Undetermined(why),
    }
}

/// Is `session_id` a LIVE registered `/flow` driver of the project of `cwd`?
///
/// `Known(false)`: no registration file, a stale one, or one that belongs to
/// another session id. `Undetermined`: an empty session id, a project
/// identity that cannot be resolved, a registration that cannot be read or
/// parsed. The caller must treat `Undetermined` like `Known(false)` — never
/// as a driver.
pub fn live_driver(
    backlog_base: &Path,
    cwd: &Path,
    session_id: &str,
    now: i64,
) -> Determination<bool> {
    if session_id.is_empty() {
        return Determination::undetermined("the hook payload carries no session id");
    }
    if !cwd.is_absolute() {
        return Determination::undetermined(format!(
            "the hook payload cwd `{}` is not absolute",
            cwd.display()
        ));
    }
    let path = match driver_file(backlog_base, cwd, session_id) {
        Determination::Known(p) => p,
        Determination::Undetermined(why) => return Determination::Undetermined(why),
    };
    let text = match harness_core::boundary::read_to_string(&path) {
        Determination::Known(Some(t)) => t,
        Determination::Known(None) => return Determination::known(false),
        Determination::Undetermined(why) => return Determination::Undetermined(why),
    };
    let v: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            return Determination::undetermined(format!(
                "could not parse the driver registration {}: {e}",
                path.display()
            ))
        }
    };
    let (Some(sid), Some(heartbeat)) = (
        v.get("session_id").and_then(|s| s.as_str()),
        v.get("heartbeat_at").and_then(|h| h.as_i64()),
    ) else {
        return Determination::undetermined(format!(
            "the driver registration {} has no string `session_id` / integer `heartbeat_at`",
            path.display()
        ));
    };
    if sid != session_id {
        // A hash collision on the file name: not this session's record.
        return Determination::known(false);
    }
    // backlog's `is_stale`: stale when `now - heartbeat_at > TTL`.
    Determination::known(now.saturating_sub(heartbeat) <= DRIVER_STALE_TTL_SECS)
}
