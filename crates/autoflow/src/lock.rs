//! Read-only view of backlog's per-project driver liveness.
//!
//! autoflow's Stop hook auto-drives `/condukt` and `/backlog`. If that auto-loop
//! fires while a `/flow` or `/backlog` driver is already running the same
//! project's queue, the queue gets driven twice. autoflow therefore stands down
//! whenever another live session is driving the same project.
//!
//! `backlog lock status --project <p>` is the signal. It used to report only the
//! *exclusive* `~/.backlog/locks/<hash-of-project>.lock`, which `/flow` held for
//! its whole loop. `/flow` no longer takes that lock (holding it monopolised the
//! queue: a second session on the same project stood down entirely, even though
//! `backlog next --claim` already guarantees two drivers get disjoint tasks).
//! It now registers *non-exclusive* presence instead, and `lock status` reports
//! the union of the two — so the answer this module reads is unchanged in shape
//! and meaning, but can now describe 2+ simultaneous drivers via the `drivers`
//! array. Everything is still asked of the `backlog` binary rather than read off
//! disk, so the path/hash scheme lives in exactly one place.
//!
//! Liveness is per-project: two unrelated projects' loops may run concurrently,
//! so this asks "is a driver active for *my* project", not "anywhere".

use std::path::Path;
use std::process::Command;

use harness_core::verdict::Determination;

use crate::backlog::{find_backlog_binary, repo_project_path};

/// True if another live session is currently driving the queue for the project
/// rooted at `cwd`.
///
/// **Cannot-determine resolves to `true` (stand down).** The two answers are
/// not symmetric: `false` starts an *unattended* auto-loop on top of whatever
/// is already running, while `true` costs one skipped tick (the Stop hook fires
/// again next turn). So a `backlog` invocation that fails to run, exits
/// non-zero, or prints something we cannot interpret is treated as "a driver
/// may be active".
///
/// The one deliberate exception is `backlog` not being installed at all. That
/// is an observation, not a failure to observe: with no `backlog` binary there
/// is no queue, no registry, and no `/flow` or `/backlog` driver to collide
/// with — and the auto-loop this guards would have nothing to double-drive.
/// Treating it as "active" would permanently disable autoflow on machines that
/// never had backlog, which is restriction without a hazard.
///
/// That exception covers only the *observed* absence. `find_backlog_binary`
/// separately reports `Undetermined` when it could not tell whether backlog is
/// installed (an unreadable plugin-cache directory), and that lands on the
/// stand-down side with every other failure to observe — the cannot-determine
/// rule above admits no carve-out it has not actually observed.
pub fn backlog_driver_active(cwd: &Path) -> bool {
    let binary = match find_backlog_binary() {
        Determination::Known(Some(b)) => b,
        // Observed: no backlog ⇒ no queue ⇒ no driver to collide with.
        Determination::Known(None) => return false,
        // Could not tell whether backlog exists ⇒ could not tell whether a
        // driver is running. Stand down, exactly like the `_ => true` below.
        Determination::Undetermined(_) => return true,
    };
    let project = repo_project_path(cwd);
    match Command::new(&binary)
        .args(["lock", "status", "--project", &project])
        .output()
    {
        Ok(out) if out.status.success() => {
            driver_active_from_status(&String::from_utf8_lossy(&out.stdout))
        }
        // Spawned but failed, or could not be spawned: we did not get an
        // answer. Stand down rather than assume the coast is clear.
        _ => true,
    }
}

/// Interpret `backlog lock status --project <p>` stdout.
///
/// * `none` → nothing is driving. This is the ONLY output that means "free":
///   backlog prints it as a positive observation, never as a fallback.
/// * a JSON object with a truthy `stale` field → only a dead holder or dead
///   registration remains → not active.
/// * any other JSON object → a live driver (this covers both an exclusive lock
///   holder and one-or-more registered drivers, and the explicitly
///   `undetermined` object backlog emits when it could not read its registry).
/// * anything else (empty, non-JSON) → we cannot interpret the answer, so we
///   report active and stand down.
///
/// `daily` carries the identical parser for the identical CLI contract.
fn driver_active_from_status(stdout: &str) -> bool {
    let trimmed = stdout.trim();
    if trimmed == "none" {
        return false;
    }
    match parse_status_json(stdout) {
        None => true,
        Some(v) => !v.get("stale").and_then(|s| s.as_bool()).unwrap_or(false),
    }
}

/// True if *this* session is driving `project`'s queue — i.e. a `/flow` (or
/// `/backlog`) loop is running from within this very Claude session. Used by
/// the PreCompact hook to decide whether to drop a resume-flow marker: we only
/// auto-resume `/flow` after a `/compact` when the flow loop was actually
/// running in this session, for this project.
///
/// Since a project can now have several concurrent drivers, "this session" is
/// matched against the top-level `session_id` (the exclusive-lock holder, or
/// the most recently heartbeated driver) AND against every entry of the
/// `drivers` array — otherwise a session that is genuinely driving would be
/// missed whenever another driver happened to have heartbeated more recently.
///
/// An empty `session_id`, an unreadable status, or no match all read as `false`
/// — not resuming is the conservative outcome here (a false positive would
/// inject a "keep driving" instruction into a session that is not driving).
/// Note the direction is the OPPOSITE of `backlog_driver_active`'s for the same
/// undetermined input, and deliberately so: there, not knowing means "someone
/// may be driving" (stand down); here, not knowing means "we cannot show this
/// session is driving", and the restrictive answer is to write no marker.
pub fn this_session_holds_lock(session_id: &str, cwd: &Path) -> bool {
    if session_id.is_empty() {
        return false;
    }
    let binary = match find_backlog_binary() {
        Determination::Known(Some(b)) => b,
        // Not installed, or we could not tell — either way nothing proves this
        // session is driving, so don't resume.
        Determination::Known(None) | Determination::Undetermined(_) => return false,
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
    let Some(v) = parse_status_json(stdout) else {
        return false;
    };
    // A stale holder/registration is not driving anything.
    if v.get("stale").and_then(|s| s.as_bool()).unwrap_or(false) {
        return false;
    }
    if v.get("session_id").and_then(|s| s.as_str()) == Some(session_id) {
        return true;
    }
    v.get("drivers")
        .and_then(|d| d.as_array())
        .is_some_and(|drivers| {
            drivers
                .iter()
                .any(|d| d.get("session_id").and_then(|s| s.as_str()) == Some(session_id))
        })
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

    /// `none` is backlog's positive observation that nothing is driving — the
    /// only output that means "free".
    #[test]
    fn driver_active_from_status_none_is_inactive() {
        assert!(!driver_active_from_status("none"));
        assert!(!driver_active_from_status("  none\n"));
    }

    /// §3: output we cannot interpret is a failure to observe, not an
    /// observation that nobody is driving. It must stand autoflow down —
    /// standing down costs one skipped tick, proceeding starts an unattended
    /// loop on top of a live driver.
    #[test]
    fn driver_active_from_status_uninterpretable_output_stands_down() {
        assert!(
            driver_active_from_status(""),
            "empty stdout is not an observation of an idle queue"
        );
        assert!(
            driver_active_from_status("not json"),
            "unparseable stdout is not an observation of an idle queue"
        );
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

    /// The new contract: `/flow` announces itself with a non-exclusive
    /// registration, so liveness now arrives as `kind: driver-presence` with a
    /// `drivers` array. One or many, autoflow must stand down.
    #[test]
    fn driver_active_from_status_registered_drivers_are_active() {
        assert!(driver_active_from_status(
            r#"{"kind":"driver-presence","session_id":"a","pid":1,"project":"/p","acquired_at":0,"heartbeat_at":9,"driver_count":1,"drivers":[{"session_id":"a"}]}"#
        ));
        assert!(driver_active_from_status(
            r#"{"kind":"driver-presence","session_id":"b","pid":1,"project":"/p","acquired_at":0,"heartbeat_at":9,"driver_count":2,"drivers":[{"session_id":"a"},{"session_id":"b"}]}"#
        ));
    }

    /// backlog says so explicitly when it could not read its registry. That
    /// object deliberately carries no `stale` field, so it reads as active.
    #[test]
    fn driver_active_from_status_undetermined_object_is_active() {
        assert!(driver_active_from_status(
            r#"{"kind":"undetermined","undetermined":true,"reason":"registry unreadable"}"#
        ));
    }

    #[test]
    fn driver_active_from_status_stale_registration_is_inactive() {
        assert!(!driver_active_from_status(
            r#"{"kind":"driver-presence","session_id":"ghost","pid":1,"project":"/p","acquired_at":0,"heartbeat_at":0,"stale":true,"driver_count":0,"drivers":[]}"#
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

    /// New contract, pinned: with several concurrent drivers, only ONE of them
    /// can be the top-level `session_id`. A session that is driving must still
    /// be recognised from the `drivers` array, or it would lose its
    /// resume-after-compact marker purely because a peer heartbeated later.
    #[test]
    fn holds_lock_from_status_matches_any_registered_driver() {
        let two_drivers = r#"{"kind":"driver-presence","session_id":"sess-b","pid":1,"project":"/p","acquired_at":0,"heartbeat_at":9,"driver_count":2,"drivers":[{"session_id":"sess-a","heartbeat_at":8},{"session_id":"sess-b","heartbeat_at":9}]}"#;
        assert!(
            holds_lock_from_status(two_drivers, "sess-b"),
            "the most recently heartbeated driver is recognised"
        );
        assert!(
            holds_lock_from_status(two_drivers, "sess-a"),
            "a concurrently registered driver must also be recognised"
        );
        assert!(
            !holds_lock_from_status(two_drivers, "sess-c"),
            "a session that is not driving must not be recognised"
        );
    }

    /// A stale registration is not a driving session, even if the id matches.
    #[test]
    fn holds_lock_from_status_ignores_a_stale_record() {
        assert!(!holds_lock_from_status(
            r#"{"session_id":"sess-a","pid":1,"project":"/p","acquired_at":0,"stale":true}"#,
            "sess-a"
        ));
    }

    #[test]
    fn empty_session_id_never_holds_lock() {
        assert!(!this_session_holds_lock("", Path::new("/p")));
    }
}
