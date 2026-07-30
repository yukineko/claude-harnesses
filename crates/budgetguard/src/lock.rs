//! A tiny cross-platform advisory lock for serializing the ledger
//! read-modify-write across concurrent sessions.
//!
//! The daily ledger is a single shared file updated on every Stop. Without
//! serialization, two sessions that Stop at the same moment each load → record →
//! save and the last writer clobbers the other's entry (lost update), silently
//! under-counting the day total and letting the daily block fail open.
//!
//! We use an `O_EXCL` lock file (`OpenOptions::create_new`) — an atomic
//! exclusive create that works the same on Unix and Windows without any external
//! crate. Acquisition spins with a short backoff up to a bounded timeout; a lock
//! left behind by a crashed process is stolen once it is older than
//! `STALE_AFTER`. Release removes the file (also on `Drop`, so a panic in the
//! critical section can't strand the lock).
//!
//! Acquisition is bounded so the hook never hangs, but a bounded WAIT is not a
//! licence to proceed unserialized. Failing to acquire is reported (`held()`),
//! not absorbed: the caller declines the read-modify-write and treats the day
//! total as undetermined. Previously this module proceeded anyway "rather than
//! ever blocking a turn" — which walked straight into the lost update described
//! above, and an under-counted day total reads as headroom. A gate that cannot
//! serialize its own accounting has not measured the day; it has guessed it.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// A held lock; releases (removes the lock file) on drop.
pub struct LedgerLock {
    path: PathBuf,
    /// False when we proceeded without truly holding the lock (timed out). Drop
    /// then must not remove a file it doesn't own.
    held: bool,
}

/// Steal a lock file older than this (its owner is assumed dead).
const STALE_AFTER: Duration = Duration::from_secs(30);
/// Give up trying to acquire after this and proceed best-effort.
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
        self.held
    }

    /// `acquire` with an injectable timeout so contention is testable without
    /// spending the full `ACQUIRE_TIMEOUT` in the suite.
    pub fn acquire_with_timeout(state_dir: &Path, timeout: Duration) -> Self {
        let path = Self::lock_path(state_dir);
        let _ = std::fs::create_dir_all(state_dir);
        let start = Instant::now();

        loop {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(_) => return LedgerLock { path, held: true },
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    // Owner looks dead AND we managed to remove its lock: retry
                    // create immediately. If the removal fails (e.g. a races/perm
                    // issue) we fall through to the timeout-bounded backoff below
                    // rather than hot-spinning forever on a file we can't delete.
                    if Self::is_stale(&path) && std::fs::remove_file(&path).is_ok() {
                        continue;
                    }
                    if start.elapsed() >= timeout {
                        // Report the failure; do NOT pretend it was acquired.
                        return LedgerLock { path, held: false };
                    }
                    std::thread::sleep(BACKOFF);
                }
                // Any other error (e.g. permissions): don't block the turn.
                Err(_) => return LedgerLock { path, held: false },
            }
        }
    }

    fn is_stale(path: &Path) -> bool {
        let meta = match std::fs::metadata(path) {
            Ok(meta) => meta,
            // Genuinely gone between attempts — acquirable.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return true,
            // Any OTHER error (a permission problem, a non-directory component
            // in the path) means we could not tell whether the owner is alive.
            // Treating that as "vanished" would steal a LIVE session's lock and
            // re-create the lost update the lock exists to prevent.
            Err(_) => return false,
        };
        match meta.modified() {
            Ok(mtime) => mtime
                .elapsed()
                .map(|age| age >= STALE_AFTER)
                .unwrap_or(false),
            Err(_) => false,
        }
    }
}

impl Drop for LedgerLock {
    fn drop(&mut self) {
        if self.held {
            let _ = std::fs::remove_file(&self.path);
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

    /// A fresh lock file belongs to a live owner. We must report that we do not
    /// hold the lock rather than quietly proceeding as though we did.
    #[test]
    fn a_contended_lock_is_reported_unheld() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path()).unwrap();
        std::fs::write(LedgerLock::lock_path(dir.path()), b"").unwrap();
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
        std::fs::write(&path, b"").unwrap();
        drop(LedgerLock::acquire_with_timeout(
            dir.path(),
            Duration::from_millis(50),
        ));
        assert!(path.exists(), "the owner's lock file must survive");
    }

    /// `metadata` failing is not evidence the owner is gone. A non-directory
    /// component in the path makes it fail with something other than NotFound;
    /// reading that as "vanished" would steal a live session's lock.
    #[test]
    fn an_unreadable_lock_is_not_assumed_stale() {
        let dir = tempfile::tempdir().unwrap();
        // `state_dir` is a regular FILE, so `state_dir/ledger.lock` cannot be
        // stat'd — the error is ENOTDIR, not NotFound.
        let not_a_dir = dir.path().join("state");
        std::fs::write(&not_a_dir, b"").unwrap();
        let path = LedgerLock::lock_path(&not_a_dir);
        let err = std::fs::metadata(&path).unwrap_err();
        assert_ne!(
            err.kind(),
            std::io::ErrorKind::NotFound,
            "fixture must produce a non-NotFound error, else it proves nothing"
        );
        assert!(
            !LedgerLock::is_stale(&path),
            "an unreadable lock must not be assumed stale and stolen"
        );
    }

    /// CONTROL — an absent lock IS genuinely acquirable, so the assertion above
    /// is about unreadability and not a blanket `false`.
    #[test]
    fn an_absent_lock_is_stale() {
        let dir = tempfile::tempdir().unwrap();
        assert!(LedgerLock::is_stale(&dir.path().join("nope.lock")));
    }
}
