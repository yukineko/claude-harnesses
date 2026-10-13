//! A tiny cross-platform advisory lock for serializing the ledger
//! read-modify-write across concurrent sessions.
//!
//! The daily ledger is a single shared file updated on every Stop. Without
//! serialization, two sessions that Stop at the same moment each load → record →
//! save and the last writer clobbers the other's entry (lost update), silently
//! under-counting the day total and letting the daily block fail open.
//!
//! The guard owns `ledger.lock` open under a kernel advisory lock
//! (`File::try_lock`, i.e. `flock(LOCK_EX|LOCK_NB)` on Unix). Releasing it —
//! explicitly or on `Drop`, so a panic in the critical section can't strand it —
//! closes the file and NEVER unlinks the path. A crashed holder's lock is
//! released by the kernel when its process exits, so there is no staleness
//! guess and no steal.
//!
//! This replaced an `O_EXCL` create + 30-second mtime stale-steal + unlink-on-
//! drop scheme (backlog 2ae3fa29, the mirror of parallelguard CA-01/02): two
//! waiters could both judge the same lock stale and both steal it, a live
//! holder whose file merely LOOKED old was robbed, and a slow holder's drop
//! deleted a different holder's lock by path — each one a second session inside
//! the critical section, i.e. the lost update this module exists to prevent.
//!
//! Acquisition is bounded so the hook never hangs, but a bounded WAIT is not a
//! licence to proceed unserialized. Failing to acquire — contention past the
//! timeout, or any IO error opening/locking the file — is reported (`held()`),
//! not absorbed: the caller declines the read-modify-write and treats the day
//! total as undetermined. A gate that cannot serialize its own accounting has
//! not measured the day; it has guessed it.

use std::fs::{File, TryLockError};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// A held lock; releases (closes the locked file, never unlinks it) on drop.
pub struct LedgerLock {
    /// `Some` only while this guard owns the kernel lock on `ledger.lock`.
    /// Dropping the `File` releases the lock. `None` = not held (contended past
    /// the timeout, or the lock file could not be opened/locked).
    file: Option<File>,
}

/// Give up trying to acquire after this and report the lock unheld.
const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(3);
/// Backoff between acquisition attempts.
const BACKOFF: Duration = Duration::from_millis(25);

impl LedgerLock {
    fn lock_path(state_dir: &Path) -> PathBuf {
        state_dir.join("ledger.lock")
    }

    /// Acquire the ledger lock for `state_dir`. Always returns a guard; when the
    /// lock could not be taken the guard is "unheld".
    ///
    /// An unheld guard is NOT permission to proceed as if serialized — see
    /// `held`. The caller must ask.
    pub fn acquire(state_dir: &Path) -> Self {
        Self::acquire_with_timeout(state_dir, ACQUIRE_TIMEOUT)
    }

    /// True only when this guard actually owns the lock. A caller that performs
    /// a read-modify-write on the shared ledger must check this: doing the
    /// update unserialized is what produces the lost update this module's
    /// header describes, and an under-counted day total reads as headroom.
    pub fn held(&self) -> bool {
        self.file.is_some()
    }

    /// `acquire` with an injectable timeout so contention is testable without
    /// spending the full `ACQUIRE_TIMEOUT` in the suite.
    pub fn acquire_with_timeout(state_dir: &Path, timeout: Duration) -> Self {
        const UNHELD: LedgerLock = LedgerLock { file: None };
        let path = Self::lock_path(state_dir);
        let _ = std::fs::create_dir_all(state_dir);
        // Open without truncating or ever removing: the path is shared by every
        // contender and only the kernel lock on it means "held". An open failure
        // means we cannot serialize — report unheld, never proceed.
        let file = match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
        {
            Ok(f) => f,
            Err(_) => return UNHELD,
        };
        let start = Instant::now();
        loop {
            match file.try_lock() {
                Ok(()) => return LedgerLock { file: Some(file) },
                Err(TryLockError::WouldBlock) => {
                    if start.elapsed() >= timeout {
                        // Report the failure; do NOT pretend it was acquired.
                        return UNHELD;
                    }
                    std::thread::sleep(BACKOFF);
                }
                // Any other error: whether we would be serialized is unknown.
                Err(TryLockError::Error(_)) => return UNHELD,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ANTI-VACUITY CONTROL — an uncontended lock is genuinely held, so the
    /// contention assertions below are about contention and not about the
    /// fixture always reporting `false`.
    #[test]
    fn an_uncontended_lock_is_held() {
        let dir = tempfile::tempdir().unwrap();
        let guard = LedgerLock::acquire(dir.path());
        assert!(guard.held(), "an uncontended lock must be acquired");
    }

    /// A live owner holds the kernel lock. We must report that we do not hold
    /// the lock rather than quietly proceeding as though we did.
    #[test]
    fn a_contended_lock_is_reported_unheld() {
        let dir = tempfile::tempdir().unwrap();
        let owner = LedgerLock::acquire(dir.path());
        assert!(owner.held(), "fixture: the live owner must hold the lock");
        let guard = LedgerLock::acquire_with_timeout(dir.path(), Duration::from_millis(50));
        assert!(
            !guard.held(),
            "a lock held by a live owner must not be reported as acquired"
        );
    }

    /// The guard must not delete a lock file it never owned — that would hand
    /// the live owner's lock to the next arrival.
    #[test]
    fn dropping_an_unheld_guard_leaves_the_owners_lock_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let path = LedgerLock::lock_path(dir.path());
        let owner = LedgerLock::acquire(dir.path());
        assert!(owner.held(), "fixture: the live owner must hold the lock");
        drop(LedgerLock::acquire_with_timeout(
            dir.path(),
            Duration::from_millis(50),
        ));
        assert!(path.exists(), "the owner's lock file must survive");
        assert!(
            !LedgerLock::acquire_with_timeout(dir.path(), Duration::from_millis(50)).held(),
            "the owner must still hold the lock after an unheld guard dropped"
        );
    }

    /// An IO error opening the lock file is not permission to proceed: whether
    /// we would be serialized is unknown, so the guard must be unheld. A
    /// non-directory component in the path makes the open fail.
    #[test]
    fn an_unopenable_lock_is_reported_unheld() {
        let dir = tempfile::tempdir().unwrap();
        // `state_dir` is a regular FILE, so `state_dir/ledger.lock` cannot be
        // created — the error is ENOTDIR.
        let not_a_dir = dir.path().join("state");
        std::fs::write(&not_a_dir, b"").unwrap();
        assert!(
            std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(false)
                .open(LedgerLock::lock_path(&not_a_dir))
                .is_err(),
            "fixture must make the lock file unopenable, else it proves nothing"
        );
        assert!(
            !LedgerLock::acquire_with_timeout(&not_a_dir, Duration::from_millis(50)).held(),
            "an unopenable lock must be reported unheld"
        );
    }

    /// A lock whose holder has gone (its guard dropped — the kernel released the
    /// lock) is acquirable, even though the lock FILE is still there: release
    /// never unlinks. Control for the contended case above.
    #[test]
    fn a_released_lock_is_reacquirable_though_the_file_remains() {
        let dir = tempfile::tempdir().unwrap();
        drop(LedgerLock::acquire(dir.path()));
        assert!(LedgerLock::lock_path(dir.path()).exists());
        assert!(LedgerLock::acquire_with_timeout(dir.path(), Duration::from_millis(50)).held());
    }
}

/// Reproduction tests for backlog 2ae3fa29 (mirror of parallelguard
/// CA-parallelguard-01/02). RED on the former O_EXCL + mtime-steal scheme.
#[cfg(test)]
mod backlog_2ae3fa29 {
    use super::*;
    use std::time::SystemTime;

    #[test]
    fn guard_drop_never_removes_a_different_holders_lock() {
        let dir = tempfile::tempdir().unwrap();
        let a = LedgerLock::acquire(dir.path());
        assert!(a.held(), "control: uncontended lock is held");
        let lock_path = LedgerLock::lock_path(dir.path());
        // A second, live holder now occupies the same path.
        std::fs::remove_file(&lock_path).unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)
            .unwrap();
        drop(a);
        assert!(
            lock_path.exists(),
            "dropping guard A deleted a lockfile belonging to a different live holder"
        );
    }

    #[test]
    fn concurrent_contenders_never_both_hold_an_abandoned_lock() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::{Arc, Barrier};
        const THREADS: usize = 24;
        for iteration in 0..40 {
            let dir = tempfile::tempdir().unwrap();
            let lock_path = LedgerLock::lock_path(dir.path());
            std::fs::write(&lock_path, b"").unwrap();
            std::fs::OpenOptions::new()
                .write(true)
                .open(&lock_path)
                .unwrap()
                .set_modified(SystemTime::now() - Duration::from_secs(3600))
                .unwrap();
            let concurrent = Arc::new(AtomicUsize::new(0));
            let max_concurrent = Arc::new(AtomicUsize::new(0));
            let barrier = Arc::new(Barrier::new(THREADS));
            std::thread::scope(|s| {
                for _ in 0..THREADS {
                    let (c, m, b) = (concurrent.clone(), max_concurrent.clone(), barrier.clone());
                    let d = dir.path().to_path_buf();
                    s.spawn(move || {
                        b.wait();
                        let g = LedgerLock::acquire_with_timeout(&d, Duration::from_secs(5));
                        if g.held() {
                            let now = c.fetch_add(1, Ordering::SeqCst) + 1;
                            m.fetch_max(now, Ordering::SeqCst);
                            std::thread::sleep(Duration::from_millis(20));
                            c.fetch_sub(1, Ordering::SeqCst);
                        }
                        drop(g);
                    });
                }
            });
            let observed = max_concurrent.load(Ordering::SeqCst);
            assert!(
                observed <= 1,
                "over-admission on iteration {iteration}: {observed} guards held the lock at once"
            );
        }
    }
}

/// Independent RED regression tests for backlog 2ae3fa29 (mirror of parallelguard
/// ca_parallelguard_01/02). Not ignored: they fail while the O_EXCL + mtime-steal
/// + unlink-by-path scheme is in use.
#[cfg(test)]
mod ca_budgetguard_lock {
    use super::*;
    use std::time::SystemTime;

    fn age_lock(path: &Path) {
        std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(3600))
            .unwrap();
    }

    /// A is a live holder whose lock file merely looks stale (mtime > 30s). B
    /// steals it. When A drops, it must not unlink B's lock; C must stay out.
    #[test]
    fn ca_budgetguard_lock_01() {
        let dir = tempfile::tempdir().unwrap();
        let a = LedgerLock::acquire(dir.path());
        assert!(a.held(), "control: A acquires an uncontended lock");
        age_lock(&LedgerLock::lock_path(dir.path()));
        let b = LedgerLock::acquire_with_timeout(dir.path(), Duration::from_millis(200));
        assert!(
            !(a.held() && b.held()),
            "A and B both hold the ledger lock at once (B stole a live holder's stale-looking lock)"
        );
        drop(a);
        let c = LedgerLock::acquire_with_timeout(dir.path(), Duration::from_millis(100));
        assert!(
            !(b.held() && c.held()),
            "C acquired the lock while B still holds it: A's Drop unlinked B's lock by path"
        );
        drop(b);
    }

    /// 24 threads race for an abandoned lock; count concurrent holders inside
    /// the critical section.
    #[test]
    fn ca_budgetguard_lock_02() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::{Arc, Barrier};
        const THREADS: usize = 24;
        for iteration in 0..40 {
            let dir = tempfile::tempdir().unwrap();
            let lock_path = LedgerLock::lock_path(dir.path());
            std::fs::write(&lock_path, b"").unwrap();
            age_lock(&lock_path);
            let concurrent = Arc::new(AtomicUsize::new(0));
            let max_concurrent = Arc::new(AtomicUsize::new(0));
            let barrier = Arc::new(Barrier::new(THREADS));
            std::thread::scope(|s| {
                for _ in 0..THREADS {
                    let (c, m, b) = (concurrent.clone(), max_concurrent.clone(), barrier.clone());
                    let d = dir.path().to_path_buf();
                    s.spawn(move || {
                        b.wait();
                        let g = LedgerLock::acquire_with_timeout(&d, Duration::from_secs(5));
                        if g.held() {
                            let now = c.fetch_add(1, Ordering::SeqCst) + 1;
                            m.fetch_max(now, Ordering::SeqCst);
                            std::thread::sleep(Duration::from_millis(20));
                            c.fetch_sub(1, Ordering::SeqCst);
                        }
                        drop(g);
                    });
                }
            });
            let observed = max_concurrent.load(Ordering::SeqCst);
            assert!(
                observed <= 1,
                "ca_budgetguard_lock_02 iteration {iteration}: {observed} holders inside the critical section at once"
            );
        }
    }
}
