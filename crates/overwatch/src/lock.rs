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
//! concurrent `begin()` calls serialize instead of racing. If the lock cannot be
//! acquired within the deadline the acquire reports "not held" (logged) and
//! never panics. What the caller does then differs by entry point: the strict
//! callers (`acquire_strict`, used by the `lease` mutators and
//! `control::reassign`) FAIL CLOSED with a non-zero exit, the
//! `store::record_changeset_and_detect` / `store::append_merge_conflict`
//! mutators also call `acquire_strict` and return `Err` (never a success-shaped
//! `Ok`), the remaining `acquire_or_skip` callers (the other `store::`
//! mutators) skip their read-modify-write, and only the
//! test-only wait variant `acquire` proceeds unlocked.

use crate::store;
use harness_core::verdict::Determination;
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
/// was not held (timeout, or acquisition error) and
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

/// Whether the process `pid` is alive — as three answers, not two.
///
/// `Known(true)`/`Known(false)` are positive observations (the OS answered).
/// `Undetermined` is "I could not ask": `kill` could not be spawned (empty
/// `PATH`, denied exec) or was killed by a signal, so there is no exit code to
/// read. The previous `.status().map(..).unwrap_or(false)` mapped that opacity
/// to `false` = "the holder is dead", which is the fail-open the caller's reap
/// arm turns into stealing a *live* holder's lock. Routing through
/// [`harness_core::boundary::run`] keeps the spawn/signal failure as
/// `Undetermined`; `map` carries the exit code (`0` == alive) only when the
/// process actually ran.
fn pid_alive(pid: u32) -> Determination<bool> {
    #[cfg(target_os = "linux")]
    {
        if Path::new(&format!("/proc/{pid}")).exists() {
            return Determination::known(true);
        }
    }
    let mut cmd = std::process::Command::new("kill");
    cmd.args(["-0", &pid.to_string()]);
    harness_core::boundary::run(&mut cmd).map(|out| out.code() == 0)
}

/// Lock file path for a project's lease registry — sits beside `leases.json`,
/// so unrelated projects (different main worktree root -> different
/// project-key storage root) never share a lock, while every checkout of ONE
/// repository (main checkout and its linked worktrees) shares the same one.
fn lock_path(cwd: &Path) -> anyhow::Result<PathBuf> {
    Ok(store::leases_path(cwd)?.with_extension("lock"))
}

fn read_info(path: &Path) -> Option<LockInfo> {
    let txt = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&txt).ok()
}

/// Why [`LeaseLock::acquire_strict`] did not return a held guard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LockError {
    /// The lock path could not be resolved (e.g. unparseable `.git` file), so
    /// the lease root is UNDETERMINED. No lock was attempted; nothing is known
    /// about contention.
    PathUndetermined(String),
    /// A lock path exists but a peer held the lock for the whole deadline
    /// (genuine contention, or a holder whose liveness could not be
    /// determined). Retrying later may succeed.
    NotAcquired,
    /// Acquisition hit an I/O failure (the lock dir could not be created, the
    /// temp lock could not be created/written, or the publish link failed for
    /// a reason other than "already exists"). No peer is known to hold
    /// anything; the lock dir is unusable, so this is NOT contention and a
    /// retry will not clear it. The payload is the failure.
    Io(String),
}

impl std::fmt::Display for LockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LockError::PathUndetermined(e) => write!(
                f,
                "could not resolve the lease root / lock path ({e}); lock state undetermined"
            ),
            LockError::NotAcquired => write!(f, "could not take the lease lock (contended)"),
            LockError::Io(e) => write!(
                f,
                "could not take the lease lock: I/O failure ({e}); lock state undetermined"
            ),
        }
    }
}

impl std::error::Error for LockError {}

impl LeaseLock {
    /// Default bounded wait before reporting the lock as not acquired. Generous enough that
    /// a normal `begin()` RMW cycle (a few file ops) always releases well
    /// within it.
    pub(crate) const DEADLINE: Duration = Duration::from_secs(10);

    /// Acquire the lease-registry lock, waiting (bounded) for any live holder
    /// to release. Reaps a stale lock whose owner pid is gone. Never fails: on
    /// timeout it logs and returns an unlocked guard (fail-soft).
    ///
    /// `#[allow(dead_code)]`: every production mutator migrated off this
    /// wait-variant — the `store::` mutators to the hard-skip
    /// [`LeaseLock::acquire_or_skip`], the lease/control mutators to the
    /// fail-closed [`LeaseLock::acquire_strict`] — proceeding under an
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
    /// that as unsafe to mutate. Test-only: production callers use
    /// [`LeaseLock::acquire_strict`] / [`LeaseLock::acquire_or_skip`], which
    /// never hand back an unheld guard.
    #[cfg(test)]
    pub fn held(&self) -> bool {
        self.path.is_some()
    }

    /// Fallible acquire: returns `Some(guard)` only when the lock is genuinely
    /// HELD, and `None` when the lock was not acquired (timeout under
    /// contention, path-resolution failure, or an I/O error) — a treat-as-held
    /// **hard-skip**. Lets a caller SKIP its load->check->save instead of
    /// mutating unlocked, which under pathological contention is what lets two
    /// timed-out writers both proceed and double-claim (last-writer-wins).
    /// Never panics. Uses the same [`LeaseLock::DEADLINE`] as
    /// [`LeaseLock::acquire`]. Live callers: the `store::` registry /
    /// check-then-append mutators (`append_merge_conflict_resolution`,
    /// `mark_*_merged`, `prune_stale_changesets`,
    /// the review-findings compaction) and `audit_round_cli::close_at`. The
    /// `lease::{begin, run, end, heartbeat, reap}` mutators and
    /// `control::reassign` use [`LeaseLock::acquire_strict`] (via
    /// `lease::lock_or_bail`) instead, so they can tell the two failures apart;
    /// `store::record_changeset_and_detect` and `store::append_merge_conflict`
    /// call [`LeaseLock::acquire_strict`] directly and return `Err` on either
    /// failure (they must not report success for a skipped mutation).
    pub fn acquire_or_skip(cwd: &Path) -> Option<Self> {
        Self::acquire_strict(cwd).ok()
    }

    /// Like [`LeaseLock::acquire_or_skip`] but keeps the three failure causes
    /// apart: [`LockError::PathUndetermined`] (the project root / lock path
    /// could not be resolved, so no lock was ever attempted),
    /// [`LockError::NotAcquired`] (a peer held the lock for the whole
    /// deadline) and [`LockError::Io`] (acquisition hit an I/O failure; no peer
    /// is known to hold anything). Callers that report the outcome must not
    /// name an undetermined path or an I/O failure as contention.
    pub fn acquire_strict(cwd: &Path) -> Result<Self, LockError> {
        Self::acquire_strict_with_deadline(cwd, Self::DEADLINE)
    }

    /// Deadline-parameterized [`LeaseLock::acquire_strict`]. The hard-skip
    /// check-then-append paths ([`crate::store::append_disposition`],
    /// [`crate::store::append_merge_conflict_resolution`]) call this so both
    /// production (the 10s default) and the wedged-holder regression test (a
    /// short deadline) drive the SAME code, and so they can report contention
    /// and an undetermined lock (path / I/O failure) as different outcomes.
    pub(crate) fn acquire_strict_with_deadline(
        cwd: &Path,
        deadline: Duration,
    ) -> Result<Self, LockError> {
        let path = match lock_path(cwd) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("overwatch: lease lock path undetermined ({e}); lock not acquired");
                return Err(LockError::PathUndetermined(e.to_string()));
            }
        };
        Self::try_acquire_at(path, deadline)
    }

    /// Core of [`LeaseLock::acquire_or_skip`] against an explicit lock `path`:
    /// runs the same acquire and maps the unheld guard to
    /// `None`. Test-only: the tests drive it with a short deadline against a
    /// self-contained temp path (the public API goes through
    /// [`LeaseLock::try_acquire_at`] via [`LeaseLock::acquire_strict`]).
    #[cfg(test)]
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
    /// the process-global `$HOME` env var. Collapses every failure to an unheld
    /// guard; [`LeaseLock::try_acquire_at`] keeps the cause.
    fn acquire_at(path: PathBuf, deadline: Duration) -> Self {
        Self::try_acquire_at(path, deadline).unwrap_or(LeaseLock { path: None })
    }

    /// [`LeaseLock::acquire_at`] with the failure cause kept:
    /// [`LockError::NotAcquired`] ONLY when a peer held the lock past
    /// `deadline`; every I/O failure is [`LockError::Io`], never contention.
    fn try_acquire_at(path: PathBuf, deadline: Duration) -> Result<Self, LockError> {
        fn io_fail(msg: String) -> LockError {
            eprintln!("overwatch: {msg}; lock not acquired");
            LockError::Io(msg)
        }
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                return Err(io_fail(format!(
                    "could not create lock dir {} ({e})",
                    parent.display()
                )));
            }
        }

        // Fully write our lock contents to a private temp file first, then
        // publish it atomically via hard link. A concurrent reader can never
        // observe a partial lock at the final path.
        let info = LockInfo {
            pid: std::process::id(),
            acquired_at: now_unix(),
        };
        let json = serde_json::to_string(&info)
            .map_err(|e| io_fail(format!("could not encode lock info ({e})")))?;
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
                    if let Err(e) = f.write_all(json.as_bytes()) {
                        let _ = std::fs::remove_file(&tmp_path);
                        return Err(io_fail(format!("could not write temp lock ({e})")));
                    }
                    f.sync_all().ok();
                }
                Err(e) => {
                    return Err(io_fail(format!("could not create temp lock ({e})")));
                }
            }
        }
        let _guard = TmpGuard(&tmp_path);

        let start = Instant::now();
        loop {
            match std::fs::hard_link(&tmp_path, &path) {
                Ok(()) => return Ok(LeaseLock { path: Some(path) }),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    // Someone holds the lock. Reap it only if we can positively
                    // confirm the owner pid is gone; otherwise wait for release.
                    match read_info(&path) {
                        // Reap ONLY on a positive "the owner is gone"
                        // (`Known(false)`). A live owner (`Known(true)`) AND an
                        // UNDETERMINED liveness (`kill` unspawnable / signalled —
                        // "I could not ask the OS") both fall through to the wait
                        // arm below. Reaping on "cannot tell" would steal a live
                        // holder's lock — the exact fail-open this lock exists to
                        // close.
                        Some(existing)
                            if matches!(pid_alive(existing.pid), Determination::Known(false)) =>
                        {
                            let _ = std::fs::remove_file(&path);
                            continue;
                        }
                        _ => {
                            if start.elapsed() >= deadline {
                                eprintln!(
                                    "overwatch: lease lock contended for {:?}; \
                                     lock not acquired",
                                    deadline
                                );
                                return Err(LockError::NotAcquired);
                            }
                            std::thread::sleep(Duration::from_millis(10));
                            continue;
                        }
                    }
                }
                Err(e) => {
                    return Err(io_fail(format!(
                        "could not publish lock {} ({e})",
                        path.display()
                    )));
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
