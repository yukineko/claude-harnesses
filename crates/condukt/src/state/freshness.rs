//! `state list` freshness classification of open runs (backlog 27393d5c).

use super::{run_path, RunState};
use crate::config::Config;
use harness_core::verdict::Determination;
use std::path::Path;

/// Age (seconds) after which an open run with no live owner is shown as stale
/// by `state list` (7 days).
pub const STALE_OPEN_RUN_SECS: i64 = 7 * 24 * 60 * 60;

/// How `state list` should present an open run's liveness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunFreshness {
    /// Recently updated, or has a live owner (fresh claim / existing worktree).
    Live,
    /// Older than [`STALE_OPEN_RUN_SECS`] and no live owner; age in seconds.
    Stale(i64),
    /// Age or ownership could not be measured. Never shown as live-and-fresh.
    Undetermined,
}

/// Classify an open run. The newest of every task `updated_at` and the state
/// file's mtime is the last activity; any unreadable signal needed to rule out
/// an owner yields [`RunFreshness::Undetermined`].
pub fn run_freshness(cfg: &Config, cwd: &Path, rs: &RunState, now: i64) -> RunFreshness {
    let mtime = run_path(cfg, cwd, &rs.run_id)
        .ok()
        .and_then(|p| std::fs::metadata(p).ok())
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64);
    let newest_task = rs.tasks.iter().filter_map(|t| t.updated_at).max();
    let last = match (mtime, newest_task) {
        (Some(a), Some(b)) => a.max(b),
        (Some(a), None) | (None, Some(a)) => a,
        (None, None) => return RunFreshness::Undetermined,
    };
    // A future timestamp (clock skew) is not a measurable age.
    let age = now - last;
    if age < 0 {
        return RunFreshness::Undetermined;
    }
    if age <= STALE_OPEN_RUN_SECS {
        return RunFreshness::Live;
    }
    let has_worktree = rs
        .tasks
        .iter()
        .filter_map(|t| t.worktree.as_deref())
        .any(|w| Path::new(w).exists());
    if has_worktree {
        return RunFreshness::Live;
    }
    match crate::claim::run_has_fresh_claim(cfg, cwd, &rs.run_id, now) {
        Determination::Known(true) => RunFreshness::Live,
        Determination::Known(false) => RunFreshness::Stale(age),
        Determination::Undetermined(_) => RunFreshness::Undetermined,
    }
}
