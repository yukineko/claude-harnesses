//! "Is a driver active for this project?" — the union of the exclusive lock
//! ([`crate::lock`]) and the non-exclusive driver-presence registry
//! ([`crate::driver`]).
//!
//! `backlog lock status` is the answer three separate consumers depend on:
//!
//! * `autoflow::lock::backlog_driver_active` — stands the Stop-hook auto-loop
//!   down while a driver is running the queue.
//! * `autoflow::lock::this_session_holds_lock` — decides whether to drop a
//!   resume-flow marker after `/compact`.
//! * `daily` — skips the whole daily run while a driver is active.
//!
//! `/flow` no longer takes the exclusive lock (it registered presence instead,
//! so a second session can drive the same queue concurrently). If
//! `lock status` reported only the exclusive lock it would now answer "none"
//! while two `/flow` loops were running — silently deleting the signal those
//! three consumers read. So the status command reports the union, and the
//! rendering keeps the exact JSON contract they already parse:
//!
//! * `none` → nothing is driving.
//! * a JSON object with `"stale": true` → only a dead holder/registration.
//! * any other JSON object → a driver is active.
//!
//! The union adds `kind`, `drivers` and `driver_count` fields; the existing
//! parsers ignore unknown fields, and the new `drivers` array is what lets a
//! consumer ask "is MY session one of the active drivers" when there are
//! several (a question the single-holder lock could not represent).
//!
//! # The `progress` field — heartbeat is not progress
//!
//! A live heartbeat means only "a process refreshed a timestamp", NOT "the
//! holder is making progress". Reporting an active/heartbeat liveness object
//! WITHOUT a progress verdict is the same fail-open as the statusline empty
//! display (CLAUDE.md §1): the three consumers above read heartbeat as
//! "working". So `status_value` now takes a REQUIRED `progress:
//! Determination<Liveness>` and every branch that asserts an active/alive
//! holder — the active exclusive-lock and the driver-presence branches — embeds
//! a `"progress"` field: `"progressing"` (`Known(Progressing)`), `"stalled"`
//! (`Known(Stalled)`), or `"undetermined"` (`Undetermined`). This makes it
//! type-impossible to render a heartbeat-only liveness. The verdict is computed
//! by the SAME machinery as the reap gate ([`crate::lock::holder_progress_verdict`]
//! / `probe`): git-head + transcript signals sampled against a persisted prior
//! snapshot, so a single sample or an unreadable signal is `"undetermined"`,
//! never faked as `"progressing"` (CLAUDE.md §3). The none/stale/undetermined
//! branches do not carry a progress verdict at all: no holder is being asserted
//! alive there, so there is no liveness claim to qualify.

use harness_core::progress::Liveness;
use harness_core::verdict::Determination;
use serde_json::{json, Value};

use crate::driver::{DriverInfo, Presence};
use crate::lock::{LockInfo, LockStatus};

/// Map a holder's progress [`Determination`] to the JSON string embedded in an
/// active-liveness object's `"progress"` field. `Undetermined` maps to
/// `"undetermined"` (NOT `"progressing"`) — "I could not tell whether the
/// holder advanced" must never be laundered into "it is progressing"
/// (CLAUDE.md §3).
fn progress_str(progress: &Determination<Liveness>) -> &'static str {
    match progress {
        Determination::Known(Liveness::Progressing) => "progressing",
        Determination::Known(Liveness::Stalled) => "stalled",
        Determination::Undetermined(_) => "undetermined",
    }
}

fn driver_value(d: &DriverInfo) -> Value {
    json!({
        "session_id": d.session_id,
        "pid": d.pid,
        "project": d.project,
        "registered_at": d.registered_at,
        "heartbeat_at": d.heartbeat_at,
    })
}

fn lock_value(info: &LockInfo) -> Value {
    json!({
        "session_id": info.session_id,
        "pid": info.pid,
        "project": info.project,
        "acquired_at": info.acquired_at,
        "heartbeat_at": info.heartbeat_at,
    })
}

/// Render the combined liveness answer, or `None` for the literal `none`
/// output (nothing is driving — an observation, not a fallback).
///
/// `progress` is the holder's progress verdict (see the module docs): every
/// branch that asserts an active/alive holder embeds it as a `"progress"`
/// field, so a heartbeat-only liveness object cannot be rendered.
///
/// Precedence, most restrictive first:
/// 1. an active exclusive lock — carries `"progress"`,
/// 2. one or more live driver registrations — carries `"progress"`,
/// 3. undetermined on either side (rendered as an active-reading object, since
///    "I could not look" must not be printed as "nothing is driving"); no
///    holder is asserted alive, so no progress claim is made,
/// 4. a stale lock or stale registrations (`"stale": true` — reads as inactive),
/// 5. `none`.
pub fn status_value(
    lock: LockStatus,
    presence: Determination<Presence>,
    progress: Determination<Liveness>,
) -> Option<Value> {
    let presence = match presence {
        Determination::Known(p) => Some(p),
        Determination::Undetermined(_) => None,
    };
    let live: Vec<&DriverInfo> = presence
        .as_ref()
        .map(|p| p.live.iter().collect())
        .unwrap_or_default();
    let drivers: Vec<Value> = live.iter().map(|d| driver_value(d)).collect();

    // 1. An active exclusive lock still wins: a holder that took it is entitled
    //    to be reported the way it always was.
    if let LockStatus::Active(info) = &lock {
        let mut v = lock_value(info);
        merge(
            &mut v,
            json!({
                "kind": "exclusive-lock",
                "driver_count": drivers.len(),
                "drivers": drivers,
                "progress": progress_str(&progress),
            }),
        );
        return Some(v);
    }

    // 2. Live registrations. The top-level identity fields describe the most
    //    recently heartbeated driver so single-driver consumers see exactly the
    //    shape they saw before; `drivers` carries the full set.
    if let Some(first) = live.first() {
        let mut v = driver_value(first);
        merge(
            &mut v,
            json!({
                "kind": "driver-presence",
                // `acquired_at` mirrors the lock's field name so a consumer
                // reading either shape finds a start time under one key.
                "acquired_at": first.registered_at,
                "driver_count": drivers.len(),
                "drivers": drivers,
                "progress": progress_str(&progress),
            }),
        );
        return Some(v);
    }

    // 3. Undetermined on either side. Deliberately rendered WITHOUT
    //    `"stale": true`, so every existing parser reads it as "a driver is
    //    active" and stands down.
    if presence.is_none() {
        return Some(json!({
            "kind": "undetermined",
            "undetermined": true,
            "reason": "driver-presence registry could not be read",
        }));
    }
    if let LockStatus::Undetermined(why) = &lock {
        return Some(json!({
            "kind": "undetermined",
            "undetermined": true,
            "reason": why,
        }));
    }

    // 4. Only dead holders/registrations remain.
    if let LockStatus::Stale(info) = &lock {
        let mut v = lock_value(info);
        merge(&mut v, json!({ "kind": "exclusive-lock", "stale": true }));
        return Some(v);
    }
    if let Some(stale) = presence.as_ref().and_then(|p| p.stale.first()) {
        let mut v = driver_value(stale);
        merge(
            &mut v,
            json!({
                "kind": "driver-presence",
                "acquired_at": stale.registered_at,
                "stale": true,
                "driver_count": 0,
                "drivers": [],
            }),
        );
        return Some(v);
    }

    // 5. Observed: nothing is driving.
    None
}

fn merge(target: &mut Value, extra: Value) {
    let (Some(t), Some(e)) = (target.as_object_mut(), extra.as_object()) else {
        return;
    };
    for (k, v) in e {
        t.insert(k.clone(), v.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drv(session: &str, heartbeat: i64) -> DriverInfo {
        DriverInfo {
            session_id: session.to_string(),
            pid: 1,
            project: "/p".to_string(),
            registered_at: 10,
            heartbeat_at: heartbeat,
        }
    }

    fn known(live: Vec<DriverInfo>, stale: Vec<DriverInfo>) -> Determination<Presence> {
        Determination::Known(Presence { live, stale })
    }

    /// A `Known(Progressing)` progress verdict — the common "holder is alive and
    /// advancing" case for tests that are not exercising the mapping itself.
    fn progressing() -> Determination<Liveness> {
        Determination::Known(Liveness::Progressing)
    }

    /// Mirrors `autoflow::lock::driver_active_from_status` and `daily`'s
    /// identical parser, so this file pins the contract they rely on.
    fn consumer_reads_active(v: &Option<Value>) -> bool {
        match v {
            None => false,
            Some(v) => !v.get("stale").and_then(|s| s.as_bool()).unwrap_or(false),
        }
    }

    // DoD 2, count = 0.
    #[test]
    fn nothing_held_and_nobody_registered_renders_none() {
        let v = status_value(
            LockStatus::None,
            known(vec![], vec![]),
            Determination::undetermined("no holder"),
        );
        assert!(v.is_none(), "expected the literal `none`, got {v:?}");
        assert!(!consumer_reads_active(&v));
    }

    // DoD 2, count = 1.
    #[test]
    fn one_registered_driver_reads_as_active() {
        let v = status_value(
            LockStatus::None,
            known(vec![drv("sess-a", 100)], vec![]),
            progressing(),
        );
        let obj = v.clone().expect("an object, not `none`");
        assert_eq!(obj["session_id"], "sess-a");
        assert_eq!(obj["driver_count"], 1);
        assert_eq!(obj["kind"], "driver-presence");
        // An active driver-presence liveness MUST carry a progress verdict.
        assert_eq!(obj["progress"], "progressing");
        assert!(consumer_reads_active(&v), "one driver must read as active");
    }

    // DoD 2, count = 2+. The state the exclusive lock could not represent.
    #[test]
    fn two_registered_drivers_both_appear_and_read_as_active() {
        let v = status_value(
            LockStatus::None,
            known(vec![drv("sess-b", 200), drv("sess-a", 100)], vec![]),
            progressing(),
        );
        let obj = v.clone().expect("an object");
        assert_eq!(obj["driver_count"], 2);
        assert_eq!(obj["progress"], "progressing");
        let ids: Vec<&str> = obj["drivers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["session_id"].as_str().unwrap())
            .collect();
        assert!(
            ids.contains(&"sess-a") && ids.contains(&"sess-b"),
            "{ids:?}"
        );
        // The top-level identity is the most recently heartbeated driver.
        assert_eq!(obj["session_id"], "sess-b");
        assert!(consumer_reads_active(&v));
    }

    #[test]
    fn stale_registrations_only_read_as_inactive() {
        let v = status_value(
            LockStatus::None,
            known(vec![], vec![drv("ghost", 0)]),
            Determination::undetermined("holder is stale, not alive"),
        );
        let obj = v.clone().expect("an object");
        assert_eq!(obj["stale"], true);
        assert_eq!(obj["driver_count"], 0);
        // A stale registration asserts no live holder, so it carries no progress
        // claim.
        assert!(obj.get("progress").is_none());
        assert!(
            !consumer_reads_active(&v),
            "a stale registration must read as inactive, exactly like a stale lock"
        );
    }

    #[test]
    fn an_active_exclusive_lock_still_wins_and_keeps_its_shape() {
        let info = LockInfo {
            session_id: "holder".to_string(),
            pid: 7,
            project: "/p".to_string(),
            acquired_at: 5,
            heartbeat_at: 500,
        };
        let v = status_value(
            LockStatus::Active(info),
            known(vec![drv("sess-a", 100)], vec![]),
            progressing(),
        );
        let obj = v.clone().expect("an object");
        assert_eq!(obj["session_id"], "holder");
        assert_eq!(obj["acquired_at"], 5);
        assert_eq!(obj["kind"], "exclusive-lock");
        // An active exclusive lock MUST carry a progress verdict.
        assert_eq!(obj["progress"], "progressing");
        // ...and concurrently-registered drivers are still listed.
        assert_eq!(obj["driver_count"], 1);
        assert!(consumer_reads_active(&v));
    }

    // §3: an unreadable registry must not render as `none`, and must not carry
    // `stale: true` — both of those read as "nothing is driving".
    #[test]
    fn undetermined_presence_reads_as_active_not_none() {
        let v = status_value(
            LockStatus::None,
            Determination::undetermined("registry unreadable"),
            Determination::undetermined("no readable holder"),
        );
        let obj = v.clone().expect("undetermined must NOT render as `none`");
        assert_eq!(obj["undetermined"], true);
        assert!(obj.get("stale").is_none());
        // The undetermined branch asserts no live holder, so it makes no
        // progress claim (it is not a heartbeat-liveness object).
        assert!(obj.get("progress").is_none());
        assert!(
            consumer_reads_active(&v),
            "cannot-determine must make consumers stand down, not proceed"
        );
    }

    #[test]
    fn undetermined_lock_reads_as_active_not_none() {
        let v = status_value(
            LockStatus::Undetermined("corrupt lock file".to_string()),
            known(vec![], vec![]),
            Determination::undetermined("no readable holder"),
        );
        let obj = v.clone().expect("undetermined must NOT render as `none`");
        assert_eq!(obj["undetermined"], true);
        assert!(obj.get("progress").is_none());
        assert!(consumer_reads_active(&v));
    }

    // Undetermined must not be masked by a stale lock (which reads inactive).
    #[test]
    fn undetermined_presence_outranks_a_stale_lock() {
        let info = LockInfo {
            session_id: "ghost".to_string(),
            pid: 7,
            project: "/p".to_string(),
            acquired_at: 0,
            heartbeat_at: 0,
        };
        let v = status_value(
            LockStatus::Stale(info),
            Determination::undetermined("registry unreadable"),
            Determination::undetermined("no readable holder"),
        );
        assert!(consumer_reads_active(&v), "got {v:?}");
    }

    // --- F→P reproduction: an active liveness MUST carry a progress verdict,
    // and the Determination → string mapping is honest (Undetermined maps to
    // "undetermined", never "progressing"). Before the fix, `status_value` took
    // no progress argument and rendered a heartbeat-only object with no
    // "progress" field at all — the fail-open this change closes.

    #[test]
    fn active_exclusive_lock_carries_progress_verdict() {
        let info = LockInfo {
            session_id: "holder".to_string(),
            pid: 7,
            project: "/p".to_string(),
            acquired_at: 5,
            heartbeat_at: 500,
        };
        // Every Determination variant maps to its honest string.
        for (progress, expected) in [
            (Determination::Known(Liveness::Progressing), "progressing"),
            (Determination::Known(Liveness::Stalled), "stalled"),
            (
                Determination::<Liveness>::undetermined("single sample"),
                "undetermined",
            ),
        ] {
            let info = info.clone();
            let v = status_value(LockStatus::Active(info), known(vec![], vec![]), progress)
                .expect("an active lock renders an object");
            assert_eq!(
                v["progress"], expected,
                "active exclusive lock must embed the holder's progress verdict"
            );
            // The active-liveness object must ALWAYS carry a progress field —
            // heartbeat-only liveness is now type-impossible.
            assert!(
                v.get("progress").is_some(),
                "a heartbeat-only active liveness object is forbidden"
            );
        }
    }

    #[test]
    fn active_driver_presence_carries_progress_verdict() {
        for (progress, expected) in [
            (Determination::Known(Liveness::Progressing), "progressing"),
            (Determination::Known(Liveness::Stalled), "stalled"),
            (
                Determination::<Liveness>::undetermined("cannot read signals"),
                "undetermined",
            ),
        ] {
            let v = status_value(
                LockStatus::None,
                known(vec![drv("sess-a", 100)], vec![]),
                progress,
            )
            .expect("a live driver renders an object");
            assert_eq!(
                v["progress"], expected,
                "active driver-presence must embed the holder's progress verdict"
            );
        }
    }

    // §3: an indeterminate progress verdict must be reported as "undetermined",
    // never faked as "progressing".
    #[test]
    fn undetermined_progress_is_never_faked_as_progressing() {
        let v = status_value(
            LockStatus::None,
            known(vec![drv("sess-a", 100)], vec![]),
            Determination::undetermined("a single sample cannot judge progress"),
        )
        .expect("an object");
        assert_eq!(v["progress"], "undetermined");
        assert_ne!(v["progress"], "progressing");
    }
}
