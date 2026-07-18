//! File lock that serializes `lease::begin()`'s load→check→save cycle.
//!
//! overwatch's lease registry lives at `leases.json` (one per project, see
//! `store::leases_path`) and `begin()` updates it with a load→check
//! (`is_held_by_other`)→save cycle. Two sessions racing `begin()` for the same
//! key at nearly the same time can both load the same pre-claim snapshot, both
//! pass the `is_held_by_other` check, and both save — the second `save_leases`
//! clobbers the first, so both sessions believe they hold the lease
//! (TOCTOU double-claim). This module gives the registry a lock file beside it
//! so the whole load→check→save cycle is mutually exclusive.
//!
//! Ported from `condukt::lock::RunLock` (same proven design, see that module's
//! doc comment): the lock is published with a hard link (`link(2)` fails
//! `EEXIST` if the target already exists, so exactly one racer wins the
//! publish and a reader never observes a partial file), a stale lock whose
//! owner pid is gone is reaped, and the reap/retry loop is bounded. It *waits*
//! (bounded) for a live holder to release rather than failing fast, so
//! concurrent `begin()` calls serialize instead of racing. Fail-soft: if the
//! lock cannot be acquired within the deadline it degrades to proceeding
//! unlocked (logged) rather than failing the caller, and it never panics.

use crate::store;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Process-wide monotonic counter so two threads in the SAME process (identical
/// pid, and possibly an identical `now_unix_nanos()` under a coarse clock) never
/// derive the same private temp-lock name. Without it a nanos collision makes the
/// loser's `create_new` fail `AlreadyExists`, degrading it to unlocked — the exact
/// race this lock exists to prevent.
static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Serialize, Deserialize)]
struct LockInfo {
    pid: u32,
    acquired_at: i64,
}

/// RAII guard for the lease-registry lock. Held across a load→check→save
/// cycle and released (best-effort) on drop. When `path` is `None` the lock
/// was not held (fail-soft degrade after a timeout, or acquisition error) and
/// drop is a no-op.
///
/// Callers that `std::process::exit()` while still holding a live guard MUST
/// `drop(guard)` explicitly first — `process::exit` skips destructors, which
/// would otherwise leak the lock file until the next stale-pid reap.
#[must_use = "the lease lock is released as soon as this guard is dropped"]
pub struct LeaseLock {
    path: Option<PathBuf>,
}

impl Drop for LeaseLock {
    fn drop(&mut self) {
        if let Some(p) = &self.path {
            let _ = std::fs::remove_file(p);
        }
    }
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

fn pid_alive(pid: u32) -> bool {
    #[cfg(target_os = "linux")]
    {
        if Path::new(&format!("/proc/{pid}")).exists() {
            return true;
        }
    }
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Lock file path for a project's lease registry — sits beside `leases.json`,
/// so unrelated projects (different `cwd` -> different project-key storage
/// root) never share a lock.
fn lock_path(cwd: &Path) -> anyhow::Result<PathBuf> {
    Ok(store::leases_path(cwd)?.with_extension("lock"))
}

fn read_info(path: &Path) -> Option<LockInfo> {
    let txt = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&txt).ok()
}

impl LeaseLock {
    /// Default bounded wait before degrading to unlocked. Generous enough that
    /// a normal `begin()` RMW cycle (a few file ops) always releases well
    /// within it.
    pub(crate) const DEADLINE: Duration = Duration::from_secs(10);

    /// Acquire the lease-registry lock, waiting (bounded) for any live holder
    /// to release. Reaps a stale lock whose owner pid is gone. Never fails: on
    /// timeout it logs and returns an unlocked guard (fail-soft).
    ///
    /// `#[allow(dead_code)]`: every production lease/store mutator migrated to
    /// the hard-skip [`LeaseLock::acquire_or_skip`] — proceeding under an
    /// UNLOCKED guard on timeout was exactly the double-claim/double-write window
    /// that closed. This wait-variant is retained as the concurrency and
    /// wedged-holder tests' live lock (a genuinely-held guard); the bin target
    /// (which re-declares `mod lock`) and the lib both read a pub fn only reached
    /// from `#[cfg(test)]` as dead.
    #[allow(dead_code)]
    pub fn acquire(cwd: &Path) -> Self {
        Self::acquire_with_deadline(cwd, Self::DEADLINE)
    }

    /// Like [`LeaseLock::acquire`] but with an explicit deadline. See
    /// [`LeaseLock::acquire`] for why this wait-variant is `allow(dead_code)`.
    #[allow(dead_code)]
    pub fn acquire_with_deadline(cwd: &Path, deadline: Duration) -> Self {
        let path = match lock_path(cwd) {
            Ok(p) => p,
            Err(e) => {
                eprintln!(
                    "overwatch: could not resolve lease lock path ({e}); proceeding unlocked"
                );
                return LeaseLock { path: None };
            }
        };
        Self::acquire_at(path, deadline)
    }

    /// Returns `true` when this guard genuinely holds the lock. A `false` here
    /// means the lock degraded to unlocked (fail-soft after a timeout or I/O
    /// error) and any RMW performed under it may race — the caller should treat
    /// that as unsafe to mutate.
    pub fn held(&self) -> bool {
        self.path.is_some()
    }

    /// Fallible acquire: returns `Some(guard)` only when the lock is genuinely
    /// HELD, and `None` when acquisition degraded to unlocked (timeout under
    /// contention, path-resolution failure, or an I/O error) — a treat-as-held
    /// **hard-skip**. Lets a caller SKIP its load->check->save instead of
    /// mutating unlocked, which under pathological contention is what lets two
    /// timed-out writers both proceed and double-claim (last-writer-wins).
    /// Fail-soft: never panics. Uses the same [`LeaseLock::DEADLINE`] as
    /// [`LeaseLock::acquire`]. Live callers: `lease::{begin, run, end, heartbeat,
    /// reap}`, `control::reassign`, and the `store::` registry / check-then-append
    /// mutators (`append_bridged_*`, `append_disposition`, `append_merge_*`,
    /// `record_changeset_and_detect`, `mark_*_merged`, `prune_stale_changesets`,
    /// the review-findings compaction), plus `audit_round_cli::close_at`.
    pub fn acquire_or_skip(cwd: &Path) -> Option<Self> {
        let path = match lock_path(cwd) {
            Ok(p) => p,
            Err(e) => {
                eprintln!(
                    "overwatch: could not resolve lease lock path ({e}); skipping (treat as held)"
                );
                return None;
            }
        };
        Self::acquire_or_skip_at(path, Self::DEADLINE)
    }

    /// Deadline-parameterized [`LeaseLock::acquire_or_skip`]. The hard-skip
    /// check-then-append path ([`crate::store::append_disposition`]) delegates
    /// here so both production (the 10s default) and its wedged-holder
    /// regression test (a short deadline) drive the SAME skip-on-contention
    /// code. Same fail-soft mapping (a degraded/unheld guard maps to `None`).
    pub(crate) fn acquire_or_skip_with_deadline(cwd: &Path, deadline: Duration) -> Option<Self> {
        let path = match lock_path(cwd) {
            Ok(p) => p,
            Err(e) => {
                eprintln!(
                    "overwatch: could not resolve lease lock path ({e}); skipping (treat as held)"
                );
                return None;
            }
        };
        Self::acquire_or_skip_at(path, deadline)
    }

    /// Core of [`LeaseLock::acquire_or_skip`] against an explicit lock `path`:
    /// runs the same fail-soft acquire and maps the degraded (unheld) guard to
    /// `None`. Shared by the public API and the tests (which drive it with a
    /// short deadline against a self-contained temp path).
    fn acquire_or_skip_at(path: PathBuf, deadline: Duration) -> Option<Self> {
        let guard = Self::acquire_at(path, deadline);
        if guard.held() {
            Some(guard)
        } else {
            None
        }
    }

    /// Core locking mechanics against an explicit lock-file `path`, independent
    /// of `store::leases_path`'s `$HOME`-derived resolution. Split out from
    /// [`LeaseLock::acquire_with_deadline`] so tests can exercise the hard-link
    /// publish/reap/contend behavior directly against a self-contained temp
    /// path, without racing every other test in this binary that also sandboxes
    /// the process-global `$HOME` env var.
    fn acquire_at(path: PathBuf, deadline: Duration) -> Self {
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                eprintln!(
                    "overwatch: could not create lock dir {} ({e}); proceeding unlocked",
                    parent.display()
                );
                return LeaseLock { path: None };
            }
        }

        // Fully write our lock contents to a private temp file first, then
        // publish it atomically via hard link. A concurrent reader can never
        // observe a partial lock at the final path.
        let info = LockInfo {
            pid: std::process::id(),
            acquired_at: now_unix(),
        };
        let json = match serde_json::to_string(&info) {
            Ok(j) => j,
            Err(_) => return LeaseLock { path: None },
        };
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
                        eprintln!("overwatch: could not write temp lock; proceeding unlocked");
                        return LeaseLock { path: None };
                    }
                    f.sync_all().ok();
                }
                Err(e) => {
                    eprintln!("overwatch: could not create temp lock ({e}); proceeding unlocked");
                    return LeaseLock { path: None };
                }
            }
        }
        let _guard = TmpGuard(&tmp_path);

        let start = Instant::now();
        loop {
            match std::fs::hard_link(&tmp_path, &path) {
                Ok(()) => return LeaseLock { path: Some(path) },
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    // Someone holds the lock. Reap it only if we can positively
                    // confirm the owner pid is gone; otherwise wait for release.
                    match read_info(&path) {
                        Some(existing) if !pid_alive(existing.pid) => {
                            let _ = std::fs::remove_file(&path);
                            continue;
                        }
                        _ => {
                            if start.elapsed() >= deadline {
                                eprintln!(
                                    "overwatch: lease lock contended for {:?}; \
                                     proceeding unlocked (update may race)",
                                    deadline
                                );
                                return LeaseLock { path: None };
                            }
                            std::thread::sleep(Duration::from_millis(10));
                            continue;
                        }
                    }
                }
                Err(e) => {
                    eprintln!(
                        "overwatch: could not publish lock {} ({e}); proceeding unlocked",
                        path.display()
                    );
                    return LeaseLock { path: None };
                }
            }
        }
    }
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

    // Exercises `acquire_at` directly against a self-contained temp path
    // (never `store::leases_path`'s `$HOME`-derived resolution), so these
    // tests need no coordination with other modules' HOME-sandboxing tests.
    fn tmp_lock_path(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "overwatch-lock-test-{tag}-{}-{}",
            std::process::id(),
            now_unix_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("leases.lock")
    }

    #[test]
    fn acquire_then_release_allows_reacquire() {
        let path = tmp_lock_path("basic");
        {
            let g = LeaseLock::acquire_at(path.clone(), Duration::from_millis(200));
            assert!(g.path.is_some());
        }
        // guard dropped -> lock file removed -> immediate reacquire succeeds
        let g2 = LeaseLock::acquire_at(path, Duration::from_millis(200));
        assert!(g2.path.is_some());
    }

    #[test]
    fn second_acquire_times_out_while_first_is_held() {
        let path = tmp_lock_path("contend");
        let _held = LeaseLock::acquire_at(path.clone(), Duration::from_millis(200));
        assert!(_held.path.is_some());
        // A second acquire (same pid, but the path is already published) must
        // degrade to unlocked within its short deadline rather than hang.
        let g2 = LeaseLock::acquire_at(path, Duration::from_millis(100));
        assert!(g2.path.is_none());
    }

    #[test]
    fn stale_lock_from_dead_pid_is_reaped() {
        let path = tmp_lock_path("stale");
        // A pid that is virtually guaranteed not to be alive.
        let info = LockInfo {
            pid: 999_999,
            acquired_at: now_unix(),
        };
        std::fs::write(&path, serde_json::to_string(&info).unwrap()).unwrap();

        let g = LeaseLock::acquire_at(path, Duration::from_millis(500));
        assert!(g.path.is_some(), "stale lock should have been reaped");
    }

    #[test]
    fn held_reflects_acquisition_state() {
        let path = tmp_lock_path("held");
        let g = LeaseLock::acquire_at(path, Duration::from_millis(200));
        assert!(g.held(), "a freshly-published lock must report held()");
    }

    // Wedge a holder, then attempt `acquire_or_skip` against the SAME lock path
    // with a short deadline: it must hard-skip (`None`) rather than hand out an
    // unheld guard. A guarded RMW modeled as a closure gated on `Some` must run
    // exactly once (the holder), NOT twice — this is the window that a plain
    // `acquire` degrade would let a second writer through, double-claiming.
    #[test]
    fn acquire_or_skip_hard_skips_while_first_is_held() {
        use std::sync::atomic::{AtomicU32, Ordering as AtomicOrdering};

        let path = tmp_lock_path("skip");
        let writes = AtomicU32::new(0);
        // A guarded RMW: it only mutates when handed a genuinely-held guard.
        let guarded_rmw = |lock: Option<LeaseLock>| {
            if let Some(g) = lock {
                assert!(g.held());
                writes.fetch_add(1, AtomicOrdering::Relaxed);
            }
        };

        // First writer genuinely holds the lock. Keep the guard alive across the
        // second writer's attempt to model overlap.
        let holder = LeaseLock::acquire_or_skip_at(path.clone(), Duration::from_millis(200));
        assert!(holder.is_some(), "first acquire_or_skip must be HELD");
        // Second writer contends the SAME lock with a short deadline: it must
        // hard-skip (None), NOT hand out an unheld guard that proceeds unlocked.
        let contended = LeaseLock::acquire_or_skip_at(path, Duration::from_millis(80));
        assert!(
            contended.is_none(),
            "contended acquire_or_skip must hard-skip (None), not proceed unlocked"
        );

        guarded_rmw(holder); // runs once (genuine holder)
        guarded_rmw(contended); // None -> skipped, no second write

        // Exactly one RMW ran; the contended writer skipped instead of
        // double-claiming (last-writer-wins).
        assert_eq!(writes.load(AtomicOrdering::Relaxed), 1);
    }
}
