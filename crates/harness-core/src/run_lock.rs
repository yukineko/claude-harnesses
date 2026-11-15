//! The run-state lock protocol, shared by every process that read-modify-writes
//! a condukt run-state file (condukt itself and autoflow's Stop hook).
//!
//! A run's state lives at `<run_dir>/<safe_session(run_id)>.json`; its lock is
//! the file `<run_dir>/<safe_session(run_id)>.lock` ([`lock_path`]). Holding it
//! makes the whole load→mutate→save cycle mutually exclusive per run, so two
//! writers can no longer both load one snapshot and have the second save
//! clobber the first (last-writer-wins). The protocol lives here, not in
//! either binary, so the two writers cannot drift into contending different
//! files or speaking different lock formats.
//!
//! Protocol:
//!
//! - **Contents.** The lock file holds one JSON [`LockInfo`]
//!   `{"pid":<u32>,"run_id":"<lock file stem>","acquired_at":<unix secs>}`.
//!   Only `pid` is read functionally (reaping); `run_id` is diagnostic.
//! - **Publish.** The contents are fully written to a private temp file
//!   (`<lock>.lock.tmp.<pid>.<nanos>.<seq>`), then published with a hard link
//!   to the lock path. link(2) fails `EEXIST` when the target exists, so
//!   exactly one racer wins and no reader ever sees a partial lock file.
//! - **Wait.** A live holder is waited out (10ms polls) for at most the given
//!   deadline ([`DEADLINE`] in production).
//! - **Reap.** A holder is reaped (its lock removed, publish retried) ONLY on
//!   a positive "the holder pid is gone" (`kill -0` answered non-zero). A live
//!   holder, an undeterminable liveness (`kill` could not be spawned or was
//!   signalled), an unreadable lock file, or a probe skipped by the throttle
//!   ([`LIVENESS_PROBE_INTERVAL`] per holder pid) all keep waiting.
//! - **Release.** Dropping the [`LockGuard`] removes the lock file
//!   (best-effort).
//!
//! **Cannot-acquire resolves to the restrictive side.** [`acquire`] returns
//! `Err(AcquireError)` on a deadline expiry or any I/O failure; there is no way
//! to obtain a guard that does not hold the lock, so "I could not tell whether
//! a peer is mid-write" is never representable as "nobody holds it". The
//! caller must then not write. Never panics.

use crate::verdict::Determination;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Default bounded wait before an acquisition FAILS (an `Err`, never an
/// unlocked proceed). Generous enough that a normal RMW cycle (a few file ops)
/// always releases well within it.
pub const DEADLINE: Duration = Duration::from_secs(10);

/// Minimum spacing between two liveness probes of the SAME holder pid while a
/// waiter polls a held lock (backlog e9262d91). The wait loop polls every 10ms,
/// and off Linux each probe is a `kill -0` fork+exec; probing on every poll
/// meant ~100 spawns per second per waiter, which under CPU saturation slowed
/// the waiter's own acquisition. Skipping a probe only ever means "keep
/// waiting" (the reap arm needs a positive `Known(false)`), so the throttle is
/// on the restrictive side: it can delay reaping a holder that died mid-wait by
/// about this interval plus one 10ms poll (and the probe's own run time), and
/// can never steal a live holder's lock. A CHANGED holder pid is probed
/// immediately.
pub const LIVENESS_PROBE_INTERVAL: Duration = Duration::from_millis(200);

/// Process-wide monotonic counter so two threads in the SAME process (identical
/// pid, and possibly an identical nanos timestamp under a coarse clock) never
/// derive the same private temp-lock name. Without it a nanos collision makes
/// the loser's `create_new` fail `AlreadyExists`, turning a perfectly
/// acquirable lock into a spurious acquisition failure.
static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// The JSON body of a published lock file.
#[derive(Debug, Serialize, Deserialize)]
pub struct LockInfo {
    /// The holder's process id. The only field read functionally (reaping).
    pub pid: u32,
    /// The lock file's stem (`safe_session(run_id)`), for diagnostics only.
    pub run_id: String,
    /// Unix seconds at acquisition, for diagnostics only.
    pub acquired_at: i64,
}

/// The run-state lock file for run `run_id` whose state file lives in
/// `run_dir`: `<run_dir>/<safe_session(run_id)>.lock`, i.e. the state file's
/// name with `.json` replaced by `.lock`. `run_id` is sanitised exactly as the
/// state file name is, so a crafted id cannot escape `run_dir`.
pub fn lock_path(run_dir: &Path, run_id: &str) -> PathBuf {
    run_dir.join(format!("{}.lock", crate::store::safe_session(run_id)))
}

/// Why an acquisition did not produce a held lock. Every variant means the
/// lock is NOT held and the caller must not perform its write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcquireError {
    /// A live (or not provably dead) holder kept the lock past the deadline.
    Contended { path: PathBuf, deadline: Duration },
    /// An I/O or serialization step failed; the message says which.
    Io(String),
}

impl std::fmt::Display for AcquireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AcquireError::Contended { path, deadline } => write!(
                f,
                "state lock {} contended for {:?}; lock NOT acquired",
                path.display(),
                deadline
            ),
            AcquireError::Io(msg) => f.write_str(msg),
        }
    }
}

impl std::error::Error for AcquireError {}

/// RAII guard for a HELD run-state lock. Only [`acquire`] (and its
/// parameterised sibling) construct it, and only after a successful publish;
/// dropping it removes the lock file (best-effort).
#[must_use = "the run lock is released as soon as this guard is dropped"]
#[derive(Debug)]
pub struct LockGuard {
    path: PathBuf,
}

impl LockGuard {
    /// The published lock file this guard holds.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Acquire the lock at `path`, waiting at most `deadline` for a live holder.
/// Production callers pass [`DEADLINE`]. `Err` = NOT held (see module docs).
pub fn acquire(path: PathBuf, deadline: Duration) -> Result<LockGuard, AcquireError> {
    acquire_with_probe(path, deadline, None, LIVENESS_PROBE_INTERVAL)
}

/// [`acquire`] with the liveness probe's seams exposed: `kill_path` is a `PATH`
/// set on the spawned `kill` CHILD only (`None` inherits this process's
/// environment — production), and `probe_interval` is the same-holder probe
/// throttle (production: [`LIVENESS_PROBE_INTERVAL`]). They exist so tests can
/// interpose a fake `kill` and pin the throttle without mutating process-wide
/// state (backlogs 7c798579, c77cdc98).
pub fn acquire_with_probe(
    path: PathBuf,
    deadline: Duration,
    kill_path: Option<&std::ffi::OsStr>,
    probe_interval: Duration,
) -> Result<LockGuard, AcquireError> {
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return Err(AcquireError::Io(format!(
                "could not create lock dir {} ({e}); lock NOT acquired",
                parent.display()
            )));
        }
    }

    // The run id is recovered from the lock filename
    // (`<safe_session(run_id)>.lock`), since this operates on a resolved path.
    let run_id = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();
    let info = LockInfo {
        pid: std::process::id(),
        run_id,
        acquired_at: now_unix(),
    };
    let json = serde_json::to_string(&info).map_err(|e| {
        AcquireError::Io(format!(
            "could not serialize lock info ({e}); lock NOT acquired"
        ))
    })?;
    let tmp_path = path.with_extension(format!(
        "lock.tmp.{}.{}.{}",
        std::process::id(),
        now_unix_nanos(),
        TMP_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    {
        use std::io::Write;
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)
        {
            Ok(mut f) => {
                if f.write_all(json.as_bytes()).is_err() {
                    let _ = std::fs::remove_file(&tmp_path);
                    return Err(AcquireError::Io(
                        "could not write temp lock; lock NOT acquired".to_string(),
                    ));
                }
                f.sync_all().ok();
            }
            Err(e) => {
                return Err(AcquireError::Io(format!(
                    "could not create temp lock ({e}); lock NOT acquired"
                )));
            }
        }
    }
    let _guard = TmpGuard(&tmp_path);

    let start = Instant::now();
    // The holder pid we last probed and when. A new holder pid is probed
    // immediately; the same holder is re-probed at most once per
    // `probe_interval`.
    let mut last_probe: Option<(u32, Instant)> = None;
    loop {
        match std::fs::hard_link(&tmp_path, &path) {
            Ok(()) => return Ok(LockGuard { path }),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                // Reap ONLY on a positive "the owner is gone" (`Known(false)`).
                // A live owner, an UNDETERMINED liveness, an unreadable lock
                // file and a throttled probe all fall through to the wait arm:
                // reaping on "cannot tell" would steal a live holder's lock.
                let holder_gone = match read_info(&path) {
                    Some(existing) => {
                        let due = match last_probe {
                            Some((pid, at)) => {
                                pid != existing.pid || at.elapsed() >= probe_interval
                            }
                            None => true,
                        };
                        if due {
                            last_probe = Some((existing.pid, Instant::now()));
                            matches!(
                                holder_alive(existing.pid, kill_path),
                                Determination::Known(false)
                            )
                        } else {
                            false
                        }
                    }
                    None => false,
                };
                if holder_gone {
                    let _ = std::fs::remove_file(&path);
                    last_probe = None;
                    continue;
                }
                if start.elapsed() >= deadline {
                    return Err(AcquireError::Contended { path, deadline });
                }
                std::thread::sleep(Duration::from_millis(10));
                continue;
            }
            Err(e) => {
                return Err(AcquireError::Io(format!(
                    "could not publish lock {} ({e}); lock NOT acquired",
                    path.display()
                )));
            }
        }
    }
}

/// Liveness of a lock holder `pid` as seen from this process. A holder pid
/// equal to our own pid is this very process — alive by observation, so no
/// `kill` is spawned. Every other pid goes to [`pid_alive_with_env_path`].
pub fn holder_alive(pid: u32, child_path: Option<&std::ffi::OsStr>) -> Determination<bool> {
    if pid == std::process::id() {
        return Determination::known(true);
    }
    pid_alive_with_env_path(pid, child_path)
}

/// Whether the process `pid` is alive — as three answers, not two.
///
/// `Known(true)`/`Known(false)` are positive observations (the OS answered).
/// `Undetermined` is "I could not ask": `kill` could not be spawned (empty
/// `PATH`, denied exec) or was killed by a signal, so there is no exit code to
/// read. Routing through [`crate::boundary::run`] keeps that as `Undetermined`
/// instead of the "holder is dead" a `unwrap_or(false)` would make of it —
/// which the reap arm would turn into stealing a live holder's lock.
///
/// `child_path` is an optional `PATH` for the spawned `kill` CHILD only (on
/// Unix also what the bare `kill` is resolved against); `None` inherits this
/// process's environment.
pub fn pid_alive_with_env_path(
    pid: u32,
    child_path: Option<&std::ffi::OsStr>,
) -> Determination<bool> {
    #[cfg(target_os = "linux")]
    {
        if Path::new(&format!("/proc/{pid}")).exists() {
            return Determination::known(true);
        }
    }
    let mut cmd = std::process::Command::new("kill");
    cmd.args(["-0", &pid.to_string()]);
    if let Some(p) = child_path {
        cmd.env("PATH", p);
    }
    crate::boundary::run(&mut cmd).map(|out| out.code() == 0)
}

fn read_info(path: &Path) -> Option<LockInfo> {
    let txt = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&txt).ok()
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

/// Removes a temp lock file when dropped, on every exit path.
struct TmpGuard<'a>(&'a Path);
impl Drop for TmpGuard<'_> {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    #[test]
    fn run_lock_path_is_state_file_name_with_lock_extension() {
        let d = Path::new("/s/p");
        assert_eq!(
            lock_path(d, "run-1"),
            PathBuf::from("/s/p/run-1.lock"),
            "lock sits beside <run>.json"
        );
        // Sanitised exactly like the state file name: no traversal.
        assert_eq!(
            lock_path(d, "../x"),
            d.join(format!("{}.lock", crate::store::safe_session("../x")))
        );
        assert!(lock_path(d, "../x").starts_with(d));
    }

    #[test]
    fn run_lock_acquire_publishes_lockinfo_and_drop_releases() {
        let t = tmp_dir();
        let p = t.path().join("run-1.lock");
        let g = acquire(p.clone(), Duration::from_millis(200)).expect("free lock is acquired");
        assert_eq!(g.path(), p.as_path());
        let info: LockInfo = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(info.pid, std::process::id());
        assert_eq!(info.run_id, "run-1");
        drop(g);
        assert!(!p.exists(), "dropping the guard releases the lock file");
        // No temp files left behind.
        let left: Vec<_> = std::fs::read_dir(t.path()).unwrap().collect();
        assert!(left.is_empty(), "{left:?}");
    }

    #[test]
    fn run_lock_contended_past_deadline_is_err_and_leaves_holder_intact() {
        let t = tmp_dir();
        let p = t.path().join("run-1.lock");
        let holder = acquire(p.clone(), Duration::from_millis(200)).unwrap();
        let err = acquire(p.clone(), Duration::from_millis(60)).unwrap_err();
        assert!(matches!(err, AcquireError::Contended { .. }), "{err:?}");
        assert!(err.to_string().contains("contended"), "{err}");
        assert!(
            p.exists(),
            "a contended waiter must not remove the holder's lock"
        );
        drop(holder);
        assert!(acquire(p, Duration::from_millis(200)).is_ok());
    }

    #[test]
    fn run_lock_reaps_a_dead_holder() {
        let t = tmp_dir();
        let p = t.path().join("run-1.lock");
        // A pid that exited: spawn and wait a short-lived child.
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let dead = child.id();
        child.wait().unwrap();
        let info = LockInfo {
            pid: dead,
            run_id: "run-1".into(),
            acquired_at: 0,
        };
        std::fs::write(&p, serde_json::to_string(&info).unwrap()).unwrap();
        let g = acquire(p.clone(), Duration::from_secs(5)).expect("dead holder is reaped");
        let now: LockInfo = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(now.pid, std::process::id());
        drop(g);
    }

    #[test]
    fn run_lock_unreadable_lock_body_is_waited_not_reaped() {
        let t = tmp_dir();
        let p = t.path().join("run-1.lock");
        std::fs::write(&p, "not json").unwrap();
        let err = acquire(p.clone(), Duration::from_millis(60)).unwrap_err();
        assert!(matches!(err, AcquireError::Contended { .. }), "{err:?}");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "not json");
    }

    #[test]
    fn run_lock_undetermined_liveness_is_waited_not_reaped() {
        let t = tmp_dir();
        let p = t.path().join("run-1.lock");
        let info = LockInfo {
            pid: u32::MAX - 1,
            run_id: "run-1".into(),
            acquired_at: 0,
        };
        let body = serde_json::to_string(&info).unwrap();
        std::fs::write(&p, &body).unwrap();
        // An empty dir as PATH: `kill` cannot be spawned => Undetermined.
        let empty = t.path().join("nobin");
        std::fs::create_dir_all(&empty).unwrap();
        let err = acquire_with_probe(
            p.clone(),
            Duration::from_millis(60),
            Some(empty.as_os_str()),
            LIVENESS_PROBE_INTERVAL,
        )
        .unwrap_err();
        assert!(matches!(err, AcquireError::Contended { .. }), "{err:?}");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), body);
    }

    #[test]
    fn run_lock_uncreatable_dir_is_io_err() {
        let t = tmp_dir();
        let file = t.path().join("f");
        std::fs::write(&file, "").unwrap();
        let err = acquire(file.join("run-1.lock"), Duration::from_millis(60)).unwrap_err();
        assert!(matches!(err, AcquireError::Io(_)), "{err:?}");
    }
}
