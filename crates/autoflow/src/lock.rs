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

/// Whether another live session is currently driving the queue for the project
/// rooted at `cwd`.
///
/// * `Known(true)` — `backlog lock status` positively reported a live driver.
///   The caller stands down silently: that is an observation, not a guess.
/// * `Known(false)` — positively observed idle (`none`, or only a stale
///   holder/registration), or `backlog` observably not installed (no queue, no
///   registry, no `/flow` driver to collide with).
/// * `Undetermined` — every failure to observe: the plugin-cache scan could not
///   tell whether backlog is installed, `backlog` could not be spawned, exited
///   non-zero, printed output that cannot be interpreted, or explicitly reported
///   that it could not read its own registry.
///
/// `Undetermined` used to collapse into `true`, and the caller then stood down
/// *silently* — stdout empty, exit 0, which Claude Code reads as "allow the
/// stop", byte-identical to a clean pass. The user ruled that a fail-open, so
/// the third answer is now returned to the caller, which surfaces it (a block,
/// bounded by `stop_hook_active`) instead of picking a side quietly.
pub fn backlog_driver_active(cwd: &Path) -> Determination<bool> {
    match lock_status_stdout(cwd) {
        Determination::Known(None) => Determination::Known(false),
        Determination::Known(Some(stdout)) => driver_active_from_status(&stdout),
        Determination::Undetermined(why) => Determination::Undetermined(why),
    }
}

/// Run `backlog lock status --project <p>` for `cwd`'s project.
///
/// `Known(None)` is the observation that backlog is not installed;
/// `Known(Some(stdout))` is the stdout of a run that exited 0; anything else
/// (an undeterminable install, a spawn failure, a non-zero exit) is
/// `Undetermined`. The exit status is part of the answer: a checker that
/// failed is not a checker that reported "idle".
fn lock_status_stdout(cwd: &Path) -> Determination<Option<String>> {
    let binary = match find_backlog_binary() {
        Determination::Known(Some(b)) => b,
        Determination::Known(None) => return Determination::Known(None),
        Determination::Undetermined(why) => return Determination::Undetermined(why),
    };
    let project = repo_project_path(cwd);
    match Command::new(&binary)
        .args(["lock", "status", "--project", &project])
        .output()
    {
        Ok(out) if out.status.success() => {
            Determination::Known(Some(String::from_utf8_lossy(&out.stdout).into_owned()))
        }
        Ok(out) => Determination::undetermined(format!(
            "`backlog lock status --project {project}` が失敗しました ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        )),
        Err(e) => Determination::undetermined(format!(
            "`backlog lock status` を起動できませんでした ({}): {e}",
            binary.display()
        )),
    }
}

/// Interpret `backlog lock status --project <p>` stdout.
///
/// * `none` → `Known(false)`. This is the ONLY bare output that means "free":
///   backlog prints it as a positive observation, never as a fallback.
/// * a JSON object with a truthy `stale` field → only a dead holder or dead
///   registration remains → `Known(false)`.
/// * the explicit `{"undetermined": true, ...}` object backlog emits when it
///   could not read its registry → `Undetermined` (backlog itself says it does
///   not know; reading that as either answer would be a guess).
/// * any other JSON object → a live driver → `Known(true)` (an exclusive lock
///   holder or one-or-more registered drivers).
/// * anything else (empty, non-JSON, JSON that is not an object) →
///   `Undetermined`.
///
/// `daily` carries a parser for the same CLI contract.
fn driver_active_from_status(stdout: &str) -> Determination<bool> {
    let trimmed = stdout.trim();
    if trimmed == "none" {
        return Determination::Known(false);
    }
    let v = match parse_status_json(stdout) {
        Some(v) if v.is_object() => v,
        _ => {
            return Determination::undetermined(format!(
                "`backlog lock status` の出力を解釈できません: {:?}",
                truncate(trimmed)
            ))
        }
    };
    if v.get("undetermined").and_then(|u| u.as_bool()) == Some(true) {
        return Determination::undetermined(format!(
            "backlog が driver の登録状況を判定できないと報告しました: {}",
            v.get("reason")
                .and_then(|r| r.as_str())
                .unwrap_or("(理由なし)")
        ));
    }
    Determination::Known(!v.get("stale").and_then(|s| s.as_bool()).unwrap_or(false))
}

fn truncate(s: &str) -> String {
    s.chars().take(200).collect()
}

/// Whether *this* session is driving `project`'s queue — i.e. a `/flow` (or
/// `/backlog`) loop is running from within this very Claude session. Used by
/// the PreCompact hook to decide whether to drop a resume-flow marker.
///
/// Since a project can now have several concurrent drivers, "this session" is
/// matched against the top-level `session_id` (the exclusive-lock holder, or
/// the most recently heartbeated driver) AND against every entry of the
/// `drivers` array.
///
/// `Known(false)` is returned only for an observation (backlog not installed,
/// `none`, a stale record, or a readable status that does not name this
/// session). A failure to observe is `Undetermined`; the caller writes no
/// marker (a false "keep driving" injection is the worse error) but says so
/// visibly instead of staying silent. An empty `session_id` cannot be matched at
/// all, so it is `Undetermined` too.
pub fn this_session_holds_lock(session_id: &str, cwd: &Path) -> Determination<bool> {
    if session_id.is_empty() {
        return Determination::undetermined(
            "session id が無いため、このセッションが driver かどうかを照合できません",
        );
    }
    let stdout = match lock_status_stdout(cwd) {
        Determination::Known(None) => return Determination::Known(false),
        Determination::Known(Some(stdout)) => stdout,
        Determination::Undetermined(why) => return Determination::Undetermined(why),
    };
    let trimmed = stdout.trim();
    if trimmed == "none" {
        return Determination::Known(false);
    }
    match parse_status_json(&stdout) {
        Some(v) if v.is_object() => {
            if v.get("undetermined").and_then(|u| u.as_bool()) == Some(true) {
                return Determination::undetermined(format!(
                    "backlog が driver の登録状況を判定できないと報告しました: {}",
                    v.get("reason")
                        .and_then(|r| r.as_str())
                        .unwrap_or("(理由なし)")
                ));
            }
            Determination::Known(holds_lock_from_status(&stdout, session_id))
        }
        _ => Determination::undetermined(format!(
            "`backlog lock status` の出力を解釈できません: {:?}",
            truncate(trimmed)
        )),
    }
}

/// Match `session_id` against an already-parsed, interpretable status object.
/// Callers that need to distinguish "unreadable" from "not this session" go
/// through [`this_session_holds_lock`], which checks interpretability first.
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

/// Parse `backlog lock status` stdout into JSON, or `None` for
/// `none`/empty/unparseable output. Callers decide what `None` means.
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
    use harness_core::verdict::Determination;

    /// `none` is backlog's positive observation that nothing is driving — the
    /// only output that means "free".
    #[test]
    fn driver_active_from_status_none_is_inactive() {
        assert!(matches!(
            driver_active_from_status("none"),
            Determination::Known(false)
        ));
        assert!(matches!(
            driver_active_from_status("  none\n"),
            Determination::Known(false)
        ));
    }

    /// §3: output we cannot interpret is a failure to observe, not an
    /// observation that nobody is driving, and not an observation that someone
    /// is: it is `Undetermined`.
    #[test]
    fn driver_active_from_status_uninterpretable_output_is_undetermined() {
        assert!(
            matches!(
                driver_active_from_status(""),
                Determination::Undetermined(_)
            ),
            "empty stdout is not an observation of an idle queue"
        );
        assert!(
            matches!(
                driver_active_from_status("not json"),
                Determination::Undetermined(_)
            ),
            "unparseable stdout is not an observation of an idle queue"
        );
        assert!(
            matches!(
                driver_active_from_status(
                    r#"{"kind":"undetermined","undetermined":true,"reason":"registry unreadable"}"#
                ),
                Determination::Undetermined(_)
            ),
            "backlog's explicit undetermined object is not a live driver"
        );
    }

    #[test]
    fn driver_active_from_status_active_lock_is_active() {
        assert!(matches!(
            driver_active_from_status(
                r#"{"session_id":"x","pid":1,"project":"/p","acquired_at":0,"heartbeat_at":0}"#
            ),
            Determination::Known(true)
        ));
    }

    #[test]
    fn driver_active_from_status_stale_lock_is_inactive() {
        assert!(matches!(
            driver_active_from_status(
                r#"{"session_id":"x","pid":1,"project":"/p","acquired_at":0,"heartbeat_at":0,"stale":true}"#
            ),
            Determination::Known(false)
        ));
    }

    /// The new contract: `/flow` announces itself with a non-exclusive
    /// registration, so liveness now arrives as `kind: driver-presence` with a
    /// `drivers` array. One or many, autoflow must stand down.
    #[test]
    fn driver_active_from_status_registered_drivers_are_active() {
        assert!(matches!(
            driver_active_from_status(
                r#"{"kind":"driver-presence","session_id":"a","pid":1,"project":"/p","acquired_at":0,"heartbeat_at":9,"driver_count":1,"drivers":[{"session_id":"a"}]}"#
            ),
            Determination::Known(true)
        ));
        assert!(matches!(
            driver_active_from_status(
                r#"{"kind":"driver-presence","session_id":"b","pid":1,"project":"/p","acquired_at":0,"heartbeat_at":9,"driver_count":2,"drivers":[{"session_id":"a"},{"session_id":"b"}]}"#
            ),
            Determination::Known(true)
        ));
    }

    #[test]
    fn driver_active_from_status_stale_registration_is_inactive() {
        assert!(matches!(
            driver_active_from_status(
                r#"{"kind":"driver-presence","session_id":"ghost","pid":1,"project":"/p","acquired_at":0,"heartbeat_at":0,"stale":true,"driver_count":0,"drivers":[]}"#
            ),
            Determination::Known(false)
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
    fn empty_session_id_is_undetermined_never_known_true() {
        let r = this_session_holds_lock("", Path::new("/p"));
        assert!(
            !matches!(r, Determination::Known(true)),
            "an empty session never counts as holding the lock"
        );
        assert!(
            matches!(r, Determination::Undetermined(_)),
            "an unmatchable session is not an observation that it lacks the lock"
        );
    }

    /// Valid JSON that is not an object is not a lock record: it must not
    /// read as a live driver.
    #[test]
    fn driver_active_from_status_non_object_json_is_undetermined() {
        for input in ["42", r#""x""#, "[]"] {
            assert!(
                matches!(
                    driver_active_from_status(input),
                    Determination::Undetermined(_)
                ),
                "{input} must be Undetermined"
            );
        }
    }
}
