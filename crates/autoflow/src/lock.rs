//! Read-only view of backlog's per-project run lock.
//!
//! autoflow's Stop hook auto-drives `/condukt` and `/backlog`. But `/flow` and
//! `/backlog` serialize their condukt runs with the backlog lock, which autoflow
//! never consulted — so if autoflow's auto-loop fired while one of those drivers
//! held the lock, the same queue would be driven twice (double condukt
//! execution). autoflow therefore stands down whenever another *live* process
//! holds the lock for the same project.
//!
//! The backlog lock is scoped per-project (`~/.backlog/locks/<hash-of-project>.lock`,
//! written by the `backlog` binary): two unrelated projects' `/flow` loops may
//! run concurrently without conflicting, so this must ask "is a driver active
//! for *my* project", not "is any driver active anywhere" — the same distinction
//! `/flow`'s own conflict check makes. This shells out to `backlog lock status
//! --project <project>` rather than reading the lock file directly, so the
//! path/hash scheme lives in exactly one place (the `backlog` crate).

use std::path::Path;
use std::process::Command;

use crate::backlog::{find_backlog_binary, repo_project_path};

/// True if another live process currently holds the backlog run lock for the
/// project rooted at `cwd`. Fail-soft: if `backlog` is absent or errors, we
/// can't see a driver, so this reports "not active" and autoflow proceeds
/// normally.
pub fn backlog_driver_active(cwd: &Path) -> bool {
    let Some(binary) = find_backlog_binary() else {
        return false;
    };
    let project = repo_project_path(cwd);
    match Command::new(&binary)
        .args(["lock", "status", "--project", &project])
        .output()
    {
        Ok(out) if out.status.success() => {
            driver_active_from_status(&String::from_utf8_lossy(&out.stdout))
        }
        _ => false,
    }
}

/// Interpret `backlog lock status --project <p>` stdout: `none` → free; a JSON
/// object with a truthy `stale` field → dead holder (not active); any other
/// JSON object → an active live holder. Mirrors `daily`'s identical parser for
/// the same CLI contract.
fn driver_active_from_status(stdout: &str) -> bool {
    match parse_status_json(stdout) {
        None => false,
        Some(v) => !v.get("stale").and_then(|s| s.as_bool()).unwrap_or(false),
    }
}

/// True if the backlog run lock for `project` is held by *this* session — i.e.
/// a `/flow` (or `/backlog`) driver is running the queue from within this very
/// Claude session. Used by the PreCompact hook to decide whether to drop a
/// resume-flow marker — we only want to auto-resume `/flow` after a `/compact`
/// when the flow loop was actually running in this session, for this project.
/// An empty `session_id`, a missing/garbage lock, or a mismatched owner all
/// read as `false` (never resume blindly).
pub fn this_session_holds_lock(session_id: &str, cwd: &Path) -> bool {
    if session_id.is_empty() {
        return false;
    }
    let Some(binary) = find_backlog_binary() else {
        return false;
    };
    let project = repo_project_path(cwd);
    match Command::new(&binary)
        .args(["lock", "status", "--project", &project])
        .output()
    {
        Ok(out) if out.status.success() => {
            holds_lock_from_status(&String::from_utf8_lossy(&out.stdout), session_id)
        }
        _ => false,
    }
}

fn holds_lock_from_status(stdout: &str, session_id: &str) -> bool {
    match parse_status_json(stdout) {
        None => false,
        Some(v) => v.get("session_id").and_then(|s| s.as_str()) == Some(session_id),
    }
}

/// Parse `backlog lock status` stdout into the lock's JSON object, or `None`
/// for `none`/empty/unparseable output (all read as "no lock to reason about").
fn parse_status_json(stdout: &str) -> Option<serde_json::Value> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() || trimmed == "none" {
        return None;
    }
    serde_json::from_str(trimmed).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn driver_active_from_status_none_is_inactive() {
        assert!(!driver_active_from_status("none"));
        assert!(!driver_active_from_status(""));
        assert!(!driver_active_from_status("not json"));
    }

    #[test]
    fn driver_active_from_status_active_lock_is_active() {
        assert!(driver_active_from_status(
            r#"{"session_id":"x","pid":1,"project":"/p","acquired_at":0,"heartbeat_at":0}"#
        ));
    }

    #[test]
    fn driver_active_from_status_stale_lock_is_inactive() {
        assert!(!driver_active_from_status(
            r#"{"session_id":"x","pid":1,"project":"/p","acquired_at":0,"heartbeat_at":0,"stale":true}"#
        ));
    }

    #[test]
    fn holds_lock_from_status_matches_owner_only() {
        assert!(!holds_lock_from_status("none", "sess-a"));
        assert!(holds_lock_from_status(
            r#"{"session_id":"sess-a","pid":1,"project":"/p","acquired_at":0}"#,
            "sess-a"
        ));
        assert!(!holds_lock_from_status(
            r#"{"session_id":"sess-a","pid":1,"project":"/p","acquired_at":0}"#,
            "sess-b"
        ));
        assert!(!holds_lock_from_status(
            r#"{"pid":1,"project":"/p","acquired_at":0}"#,
            "sess-a"
        ));
        assert!(!holds_lock_from_status("not json", "sess-a"));
    }

    #[test]
    fn empty_session_id_never_holds_lock() {
        assert!(!this_session_holds_lock("", Path::new("/p")));
    }
}
