//! Non-exclusive driver-presence registry.
//!
//! # Why this exists
//!
//! `~/.backlog/locks/<project>.lock` (see [`crate::lock`]) is an *exclusive*
//! per-project lock. `/flow` used to take it for the whole duration of its
//! loop, which meant a second `/flow` session on the same project stood down
//! entirely — one session monopolised the whole queue. Per-task exclusivity
//! does not need that: [`crate::store::next_claim`] reserves the returned task
//! inside the same tasks-file-lock critical section that selects it, so two
//! concurrent drivers pulling from the same queue are already guaranteed
//! disjoint tasks.
//!
//! But the exclusive lock was doing a *second*, unrelated job: it was the
//! "a driver is active for this project" signal that `autoflow` (auto-loop
//! stand-down) and `daily` (skip the daily run) read via
//! `backlog lock status`. Deleting the lock from `/flow` would silently delete
//! that signal too.
//!
//! This registry replaces only that second job. Registration is
//! **non-exclusive**: any number of sessions may be registered for the same
//! project at once, each with its own session id, pid and heartbeat, and
//! registration never fails because someone else is registered. Staleness uses
//! the *same* TTL and the same heartbeat rule as the exclusive lock
//! ([`crate::lock::LOCK_STALE_TTL_SECS`]), and stale registrations are reaped
//! on the next `register`.
//!
//! # Cannot-determine is not "nobody is driving"
//!
//! The presence question has a restrictive side and a permissive side, and they
//! are not symmetric. Reporting "no driver" makes `autoflow` fire its auto-loop
//! and `daily` run its tasks; reporting "a driver is active" makes them stand
//! down. So an unreadable registry directory must NOT collapse to an empty set
//! (which reads downstream as "nobody is driving" — the permissive answer).
//! [`presence_at`] therefore returns a `Determination`: a directory that has
//! never existed is `Known(empty)` (nobody ever registered — an observed fact),
//! while any other IO failure is `Undetermined` and is rendered as an explicitly
//! undetermined status that downstream reads as "active".

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use harness_core::verdict::Determination;
use serde::{Deserialize, Serialize};

use crate::lock::{project_slug, LOCK_STALE_TTL_SECS};
use harness_core::config::base_dir;

/// One registered driver. Mirrors [`crate::lock::LockInfo`]'s field names for
/// `session_id` / `pid` / `project` / `heartbeat_at` so the union rendered by
/// `backlog lock status` has one consistent shape for existing consumers.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DriverInfo {
    pub session_id: String,
    /// OS pid of the process that last registered/heartbeated. Observability
    /// only — NOT used to judge liveness, for exactly the reason
    /// [`crate::lock::LockInfo::pid`] documents: `backlog` is a one-shot CLI, so
    /// the recorded pid is already dead by the time anyone reads the file.
    /// Liveness is judged by `heartbeat_at`.
    pub pid: u32,
    pub project: String,
    pub registered_at: i64,
    pub heartbeat_at: i64,
}

/// The observed set of registrations for a project (or for every project).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Presence {
    /// Registrations whose heartbeat is still within the TTL, most recently
    /// heartbeated first.
    pub live: Vec<DriverInfo>,
    /// Registrations whose heartbeat is older than the TTL. Reaped on the next
    /// `register` for that project.
    pub stale: Vec<DriverInfo>,
}

impl Presence {
    /// Number of live drivers. `0`, `1` and `2+` are all meaningful and all
    /// distinguishable — that is the whole point of a non-exclusive registry.
    pub fn live_count(&self) -> usize {
        self.live.len()
    }
}

fn drivers_root(base: Option<&Path>) -> PathBuf {
    match base {
        Some(b) => b.join("drivers"),
        None => base_dir("backlog").join("drivers"),
    }
}

fn project_dir(base: Option<&Path>, project: &str) -> PathBuf {
    drivers_root(base).join(project_slug(project))
}

/// Filesystem-safe per-session filename. Hashed for the same reason the project
/// slug is: a session id is opaque and must never become a path separator.
fn session_slug(session_id: &str) -> String {
    format!(
        "{:016x}",
        harness_core::hash::fnv1a64(session_id.as_bytes())
    )
}

fn driver_path(base: Option<&Path>, project: &str, session_id: &str) -> PathBuf {
    project_dir(base, project).join(format!("{}.driver", session_slug(session_id)))
}

fn now_unix() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn now_unix_nanos() -> u128 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

fn is_stale(info: &DriverInfo, now: i64) -> bool {
    now.saturating_sub(info.heartbeat_at) > LOCK_STALE_TTL_SECS
}

/// Register (or refresh) this session as a driver of `project`.
///
/// Non-exclusive by construction: the file is keyed by session id, so a second
/// session writes a *different* file and both are live at once. Re-registering
/// the same session overwrites its own file (registered_at is preserved from
/// the existing record so the registry keeps the original start time).
///
/// Reaps stale registrations for this project as a side effect, matching what
/// `lock acquire` does with a stale lock.
pub fn register_at(
    session_id: &str,
    pid: u32,
    project: &str,
    base: Option<&Path>,
) -> Result<DriverInfo> {
    let dir = project_dir(base, project);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("create driver registry dir {}", dir.display()))?;

    let now = now_unix();
    let path = driver_path(base, project, session_id);
    let registered_at = read_driver(&path).map(|i| i.registered_at).unwrap_or(now);
    let info = DriverInfo {
        session_id: session_id.to_string(),
        pid,
        project: project.to_string(),
        registered_at,
        heartbeat_at: now,
    };
    write_atomic(&path, &info)?;
    reap_stale_in(&dir, now);
    Ok(info)
}

/// Refresh this session's heartbeat.
///
/// This is an **upsert**, not a no-op-if-absent: a driver that calls heartbeat
/// is demonstrably alive right now, so if its record is missing (it was reaped
/// after a long gap between loop iterations, or it never registered) we write
/// it back. The alternative — silently doing nothing — would leave a live
/// driver unregistered and make the presence query under-report, which is the
/// permissive direction (autoflow would fire while a driver is running).
pub fn heartbeat_at(session_id: &str, pid: u32, project: &str, base: Option<&Path>) -> Result<()> {
    register_at(session_id, pid, project, base).map(|_| ())
}

/// Remove this session's registration. No-op if it is not registered.
pub fn unregister_at(session_id: &str, project: &str, base: Option<&Path>) -> Result<()> {
    let path = driver_path(base, project, session_id);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("remove driver record {}", path.display())),
    }
}

/// Observe the registrations for `project`, or (with `project = None`) for every
/// project — the cross-project scan `daily` needs.
///
/// Returns `Undetermined` when the registry cannot be read for any reason other
/// than "it has never been created". See the module docs: an unreadable
/// registry is not an empty one.
pub fn presence_at(project: Option<&str>, base: Option<&Path>) -> Determination<Presence> {
    let dirs: Vec<PathBuf> = match project {
        Some(p) => vec![project_dir(base, p)],
        None => match list_subdirs(&drivers_root(base)) {
            Determination::Known(d) => d,
            Determination::Undetermined(why) => return Determination::Undetermined(why),
        },
    };

    let now = now_unix();
    let mut presence = Presence::default();
    for dir in dirs {
        let files = match list_driver_files(&dir) {
            Determination::Known(f) => f,
            Determination::Undetermined(why) => return Determination::Undetermined(why),
        };
        for path in files {
            // A record we cannot parse is not evidence of absence. Treat it as
            // a live driver we cannot describe rather than dropping it (dropping
            // it would shrink the live set — the permissive direction).
            let Some(info) = read_driver(&path) else {
                return Determination::undetermined(format!(
                    "unreadable driver record {}",
                    path.display()
                ));
            };
            if is_stale(&info, now) {
                presence.stale.push(info);
            } else {
                presence.live.push(info);
            }
        }
    }
    // Most recently heartbeated first: `liveness::status_value` uses the head
    // of `live` as the top-level identity of the rendered status.
    presence
        .live
        .sort_by_key(|d| std::cmp::Reverse(d.heartbeat_at));
    presence
        .stale
        .sort_by_key(|d| std::cmp::Reverse(d.heartbeat_at));
    Determination::Known(presence)
}

/// `read_dir` over a directory that is *allowed* not to exist yet (nothing has
/// ever registered). Every other error is undetermined, never an empty list.
fn read_dir_entries(dir: &Path) -> Determination<Vec<PathBuf>> {
    match std::fs::read_dir(dir) {
        Ok(entries) => {
            let mut out = Vec::new();
            for e in entries {
                match e {
                    Ok(e) => out.push(e.path()),
                    Err(err) => {
                        return Determination::undetermined(format!(
                            "reading {}: {err}",
                            dir.display()
                        ))
                    }
                }
            }
            Determination::Known(out)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Determination::Known(Vec::new()),
        Err(e) => Determination::undetermined(format!("reading {}: {e}", dir.display())),
    }
}

fn list_subdirs(dir: &Path) -> Determination<Vec<PathBuf>> {
    read_dir_entries(dir).map(|paths| paths.into_iter().filter(|p| p.is_dir()).collect())
}

fn list_driver_files(dir: &Path) -> Determination<Vec<PathBuf>> {
    read_dir_entries(dir).map(|paths| {
        paths
            .into_iter()
            .filter(|p| p.extension().is_some_and(|e| e == "driver"))
            .collect()
    })
}

fn read_driver(path: &Path) -> Option<DriverInfo> {
    let txt = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&txt).ok()
}

/// Write the record fully into a private temp file, then `rename` it into
/// place, so a concurrent reader never observes a half-written record (which
/// [`presence_at`] would have to call undetermined).
fn write_atomic(path: &Path, info: &DriverInfo) -> Result<()> {
    use std::io::Write;
    let json = serde_json::to_string_pretty(info)?;
    let tmp = path.with_extension(format!("tmp.{}.{}", std::process::id(), now_unix_nanos()));
    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .with_context(|| format!("create temp driver record {}", tmp.display()))?;
        f.write_all(json.as_bytes())
            .with_context(|| format!("write temp driver record {}", tmp.display()))?;
        f.sync_all().ok();
    }
    let res = std::fs::rename(&tmp, path)
        .with_context(|| format!("publish driver record {}", path.display()));
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    res
}

/// Delete registrations in `dir` whose heartbeat is past the TTL. Mirrors the
/// stale reap `lock acquire` performs. Deleting another session's stale record
/// is safe by definition: past the TTL it can no longer be counted as live by
/// anyone, so removing the file changes no answer — it only stops the registry
/// growing without bound.
fn reap_stale_in(dir: &Path, now: i64) {
    let Determination::Known(files) = list_driver_files(dir) else {
        return;
    };
    for path in files {
        if let Some(info) = read_driver(&path) {
            if is_stale(&info, now) {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn known(p: Determination<Presence>) -> Presence {
        match p {
            Determination::Known(v) => v,
            Determination::Undetermined(why) => {
                panic!("expected Known presence, got Undetermined: {why:?}")
            }
        }
    }

    // DoD 2: liveness answers correctly at 0, 1 and 2+ concurrent drivers.
    // Under the exclusive lock, "2" was not a representable state at all —
    // the second acquire simply failed.
    #[test]
    fn presence_counts_zero_one_and_two_or_more_drivers() {
        let dir = TempDir::new().unwrap();
        let d = Some(dir.path());
        let pid = std::process::id();

        assert_eq!(
            known(presence_at(Some("/proj"), d)).live_count(),
            0,
            "no registrations → 0 live drivers"
        );

        register_at("sess-a", pid, "/proj", d).unwrap();
        assert_eq!(known(presence_at(Some("/proj"), d)).live_count(), 1);

        register_at("sess-b", pid, "/proj", d).unwrap();
        let p = known(presence_at(Some("/proj"), d));
        assert_eq!(
            p.live_count(),
            2,
            "two concurrent drivers must BOTH be registered (non-exclusive)"
        );
        let mut ids: Vec<&str> = p.live.iter().map(|i| i.session_id.as_str()).collect();
        ids.sort_unstable();
        assert_eq!(ids, vec!["sess-a", "sess-b"]);

        register_at("sess-c", pid, "/proj", d).unwrap();
        assert_eq!(known(presence_at(Some("/proj"), d)).live_count(), 3);

        // Unregistering one leaves the others live.
        unregister_at("sess-b", "/proj", d).unwrap();
        let p = known(presence_at(Some("/proj"), d));
        assert_eq!(p.live_count(), 2);
        assert!(p.live.iter().all(|i| i.session_id != "sess-b"));
    }

    // The load-bearing difference from the exclusive lock: a second registration
    // must NOT be refused. This is the monopoly the change exists to remove.
    #[test]
    fn second_driver_registration_is_never_refused() {
        let dir = TempDir::new().unwrap();
        let d = Some(dir.path());
        let pid = std::process::id();
        register_at("first", pid, "/proj", d).expect("first registers");
        register_at("second", pid, "/proj", d)
            .expect("a second concurrent driver must register, not be refused");
    }

    // DoD 2: a stale driver is reaped exactly as the exclusive lock's stale
    // holder is — same TTL, judged on heartbeat_at, not pid.
    #[test]
    fn stale_driver_is_not_live_and_is_reaped_on_next_register() {
        let dir = TempDir::new().unwrap();
        let d = Some(dir.path());
        let pid = std::process::id();

        register_at("ghost", pid, "/proj", d).unwrap();
        // Back-date the heartbeat past the TTL.
        let path = driver_path(d, "/proj", "ghost");
        let mut info = read_driver(&path).unwrap();
        info.heartbeat_at = now_unix() - LOCK_STALE_TTL_SECS - 1;
        std::fs::write(&path, serde_json::to_string(&info).unwrap()).unwrap();

        let p = known(presence_at(Some("/proj"), d));
        assert_eq!(p.live_count(), 0, "a stale driver is not live");
        assert_eq!(p.stale.len(), 1, "but it is still observed as stale");

        // A fresh registration reaps it (same as `lock acquire` reaping a stale
        // lock), leaving exactly the live one.
        register_at("fresh", pid, "/proj", d).unwrap();
        let p = known(presence_at(Some("/proj"), d));
        assert_eq!(p.live_count(), 1);
        assert_eq!(p.live[0].session_id, "fresh");
        assert!(p.stale.is_empty(), "stale record must have been reaped");
        assert!(!path.exists(), "stale record file must be gone");
    }

    // A driver that heartbeats is alive right now: if its record was reaped in
    // the meantime, heartbeat must put it back, not silently no-op (a silent
    // no-op under-reports presence, which is the permissive direction).
    #[test]
    fn heartbeat_reregisters_a_reaped_driver_and_refreshes_liveness() {
        let dir = TempDir::new().unwrap();
        let d = Some(dir.path());
        let pid = std::process::id();

        register_at("sess", pid, "/proj", d).unwrap();
        let path = driver_path(d, "/proj", "sess");
        let mut info = read_driver(&path).unwrap();
        let old_registered = info.registered_at;
        info.heartbeat_at = now_unix() - LOCK_STALE_TTL_SECS - 1;
        std::fs::write(&path, serde_json::to_string(&info).unwrap()).unwrap();
        assert_eq!(known(presence_at(Some("/proj"), d)).live_count(), 0);

        heartbeat_at("sess", pid, "/proj", d).unwrap();
        let p = known(presence_at(Some("/proj"), d));
        assert_eq!(p.live_count(), 1, "heartbeat must restore liveness");
        assert_eq!(
            p.live[0].registered_at, old_registered,
            "re-registration keeps the original start time"
        );

        // Fully removed record: heartbeat re-creates it.
        std::fs::remove_file(&path).unwrap();
        assert_eq!(known(presence_at(Some("/proj"), d)).live_count(), 0);
        heartbeat_at("sess", pid, "/proj", d).unwrap();
        assert_eq!(known(presence_at(Some("/proj"), d)).live_count(), 1);
    }

    // Registrations are per-project: a driver on another project is not a
    // driver on mine. This preserves the property the per-project lock file
    // scoping introduced.
    #[test]
    fn registrations_are_scoped_per_project() {
        let dir = TempDir::new().unwrap();
        let d = Some(dir.path());
        let pid = std::process::id();
        register_at("sess-a", pid, "/proj-a", d).unwrap();
        assert_eq!(known(presence_at(Some("/proj-a"), d)).live_count(), 1);
        assert_eq!(known(presence_at(Some("/proj-b"), d)).live_count(), 0);
        // ...but the cross-project scan (daily's "is anyone driving anywhere")
        // sees it.
        assert_eq!(known(presence_at(None, d)).live_count(), 1);

        register_at("sess-b", pid, "/proj-b", d).unwrap();
        assert_eq!(known(presence_at(None, d)).live_count(), 2);
    }

    // §3: an unreadable registry is NOT an empty registry. "I could not look"
    // must not render as "nobody is driving", because that is the answer that
    // makes autoflow fire and daily run.
    #[test]
    fn unreadable_record_is_undetermined_not_empty() {
        let dir = TempDir::new().unwrap();
        let d = Some(dir.path());
        register_at("sess", std::process::id(), "/proj", d).unwrap();
        std::fs::write(driver_path(d, "/proj", "sess"), "{ not json").unwrap();

        match presence_at(Some("/proj"), d) {
            Determination::Undetermined(_) => {}
            Determination::Known(p) => panic!(
                "a corrupt record must be Undetermined, not an empty/short live set (got {} live)",
                p.live_count()
            ),
        }
    }

    // The one absence that IS an observation: the registry has never been
    // created, so nobody has ever registered.
    #[test]
    fn never_created_registry_is_known_empty() {
        let dir = TempDir::new().unwrap();
        let d = Some(dir.path());
        assert_eq!(known(presence_at(Some("/proj"), d)).live_count(), 0);
        assert_eq!(known(presence_at(None, d)).live_count(), 0);
    }

    // Concurrency: many threads registering as distinct sessions for the same
    // project must ALL end up registered — no winner, no loser.
    #[test]
    fn concurrent_registrations_all_succeed() {
        use std::sync::{Arc, Barrier};

        let dir = TempDir::new().unwrap();
        let base = Arc::new(dir.path().to_path_buf());
        let pid = std::process::id();
        const N: usize = 16;
        let barrier = Arc::new(Barrier::new(N));

        let mut handles = Vec::with_capacity(N);
        for i in 0..N {
            let base = Arc::clone(&base);
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                register_at(&format!("sess-{i}"), pid, "/proj", Some(base.as_path()))
            }));
        }
        let mut ok = 0usize;
        for h in handles {
            if h.join().expect("thread join").is_ok() {
                ok += 1;
            }
        }
        assert_eq!(ok, N, "every concurrent registration must succeed");
        assert_eq!(
            known(presence_at(Some("/proj"), Some(base.as_path()))).live_count(),
            N
        );
    }
}
