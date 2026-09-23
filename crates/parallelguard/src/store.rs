//! Where the per-session ledger lives, and how concurrent hook processes agree
//! on it.
//!
//! Two properties this module owes the gate, both of which the obvious
//! implementation gets wrong:
//!
//! 1. **A failed write is not a successful one.** `harness_core::store::save_json`
//!    is fail-soft by contract (it swallows IO errors and returns `()`), which
//!    is right for notes and wrong here: a slot that was admitted but never
//!    recorded is a call running *outside* the count, so the very next call
//!    sees room that does not exist and the cap quietly rises. [`save`] returns
//!    `Result` so the caller must decide, in the diff, what an unwritable store
//!    means.
//!
//! 2. **A lock that cannot be taken is not an empty ledger.** [`lock`] returns
//!    `Determination`, so "another process holds it and would not let go"
//!    reaches the caller as undetermined rather than as a read of stale state.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use harness_core::verdict::Determination;

use crate::model::Inflight;

/// Env override for the state root. Honored only when non-empty and absolute —
/// a relative path would resolve against each hook process's cwd, silently
/// splitting one session's ledger across directories (the same rule
/// `harness_core::store::context_ledger_base` applies, for the same reason).
pub const ENV_STATE_DIR: &str = "PARALLELGUARD_STATE_DIR";

/// Lock acquisition budget: 400 attempts x 5 ms = up to 2 s. Sized well above
/// the handful of concurrent hook processes one session can produce, so
/// exhausting it means something is genuinely wrong rather than merely busy.
///
/// There is deliberately no staleness timeout and no steal. The lock is a
/// kernel advisory lock (`File::try_lock`, i.e. `flock(LOCK_EX|LOCK_NB)` on
/// unix) held on an open descriptor, and the kernel drops it when the
/// descriptor closes — including when the holder is SIGKILLed. A dead holder
/// therefore never outlives its process, and a live holder, however slow, is
/// never judged dead by an mtime guess. (The previous `create_new` + 30 s
/// mtime-steal scheme could delete a live holder's lockfile — its own `Drop`
/// or a second thief removed by path whatever lock sat there — admitting two
/// critical sections at once: CA-parallelguard-01/02.)
const LOCK_ATTEMPTS: u32 = 400;
const LOCK_DELAY: Duration = Duration::from_millis(5);

/// Root of the state tree: `$PARALLELGUARD_STATE_DIR`, else
/// `$HOME/.parallelguard/state`, else `./.parallelguard/state`.
#[must_use]
pub fn state_dir() -> PathBuf {
    if let Ok(raw) = std::env::var(ENV_STATE_DIR) {
        let p = PathBuf::from(&raw);
        if !raw.is_empty() && p.is_absolute() {
            return p;
        }
    }
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() => PathBuf::from(home).join(".parallelguard").join("state"),
        _ => PathBuf::from(".parallelguard").join("state"),
    }
}

/// The ledger path for one session. The session id is sanitised into a single
/// path component, so a hostile id cannot escape the state dir.
#[must_use]
pub fn session_path(root: &Path, session: &str) -> PathBuf {
    root.join("sessions").join(format!(
        "{}.json",
        harness_core::store::safe_session(session)
    ))
}

/// Seconds since the Unix epoch, or 0 if the clock is before it. Used only for
/// bookkeeping and display — never to expire a slot.
#[must_use]
pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// RAII lock guard: owns the open lockfile descriptor that carries the kernel
/// lock. Releasing is closing that descriptor (on drop, including during a
/// panic unwind, or by the kernel when the process dies). It NEVER removes
/// the lockfile path: the path is persistent, and unlinking it while another
/// process holds a lock on it would let a third process create a fresh inode
/// at the same path and lock that one concurrently.
pub struct LockGuard {
    _file: std::fs::File,
}

fn lock_path_for(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".lock");
    PathBuf::from(s)
}

/// Take the advisory lock guarding `path`'s critical section.
///
/// Opens (creating if absent, never truncating or removing) the persistent
/// lockfile `<path>.lock` and takes an exclusive kernel lock on it with
/// `File::try_lock`, retrying within `LOCK_ATTEMPTS` x `LOCK_DELAY`. The lock
/// belongs to the open descriptor, so ownership is exact: only the holder's
/// own `LockGuard` can release it, and a holder that dies releases it with its
/// process. Failure to acquire — the budget exhausted, or any IO error opening
/// or locking the file — is `Undetermined`, never a silent "assume nothing is
/// in flight".
pub fn lock(path: &Path) -> Determination<LockGuard> {
    let lock_path = lock_path_for(path);
    if let Some(parent) = lock_path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return Determination::undetermined(format!(
                "cannot create the state directory {}: {e}",
                parent.display()
            ));
        }
    }
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
    {
        Ok(f) => f,
        Err(e) => {
            return Determination::undetermined(format!(
                "cannot open the lockfile {}: {e}",
                lock_path.display()
            ))
        }
    };
    for _ in 0..LOCK_ATTEMPTS {
        match file.try_lock() {
            Ok(()) => return Determination::known(LockGuard { _file: file }),
            Err(std::fs::TryLockError::WouldBlock) => std::thread::sleep(LOCK_DELAY),
            Err(std::fs::TryLockError::Error(e)) => {
                return Determination::undetermined(format!(
                    "cannot lock the lockfile {}: {e}",
                    lock_path.display()
                ))
            }
        }
    }
    Determination::undetermined(format!(
        "the lockfile {} stayed held for the whole retry budget ({} attempts x {} ms)",
        lock_path.display(),
        LOCK_ATTEMPTS,
        LOCK_DELAY.as_millis()
    ))
}

/// Read the ledger. Absent is `Known(empty)` — a session that has run nothing
/// yet genuinely holds nothing. Unreadable or unparseable is `Undetermined`.
pub fn load(path: &Path) -> Determination<Inflight> {
    harness_core::store::load_json_determined::<Inflight>(path)
}

/// Write the ledger, reporting failure.
///
/// Atomic: the payload goes to a per-process temp sibling, is flushed, then
/// renamed over the target, so a concurrent reader sees the old file or the
/// whole new one, never a truncated middle.
pub fn save(path: &Path, value: &Inflight) -> Result<(), String> {
    use std::io::Write;

    let parent = path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", path.display()))?;
    std::fs::create_dir_all(parent)
        .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    let body = serde_json::to_string(value).map_err(|e| format!("cannot serialize ledger: {e}"))?;
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    {
        let mut f = std::fs::File::create(&tmp)
            .map_err(|e| format!("cannot create {}: {e}", tmp.display()))?;
        f.write_all(body.as_bytes())
            .map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
        f.flush()
            .map_err(|e| format!("cannot flush {}: {e}", tmp.display()))?;
    }
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!(
            "cannot rename {} over {}: {e}",
            tmp.display(),
            path.display()
        )
    })
}

/// Drop a session's ledger.
///
/// This is the turn boundary's self-heal: whatever leaked (a call the user
/// rejected, a hook killed mid-flight, a corrupt store) is gone by the next
/// turn without anyone having to intervene.
///
/// The lockfile is deliberately left in place. It needs no self-heal — a dead
/// holder's kernel lock is already gone with its process — and unlinking it
/// while a live hook holds it would let the next `lock` create a new inode at
/// the same path and lock it concurrently with that holder.
pub fn reset(path: &Path) {
    let _ = std::fs::remove_file(path);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::SlotClass;

    #[test]
    fn a_relative_state_dir_override_is_ignored() {
        // A relative root would resolve against each hook process's cwd and
        // split one session's ledger in two.
        temp_env(ENV_STATE_DIR, Some("relative/path"), || {
            assert!(state_dir().is_absolute() || state_dir().starts_with(".parallelguard"));
            assert_ne!(state_dir(), PathBuf::from("relative/path"));
        });
    }

    #[test]
    fn a_session_id_cannot_escape_the_state_dir() {
        let root = PathBuf::from("/tmp/pg");
        let p = session_path(&root, "../../etc/passwd");
        assert_eq!(p.parent(), Some(root.join("sessions").as_path()));
        // The separators are what matter: `..` may survive as literal text in
        // the file NAME (safe_session maps `/` to `_`), but never as a path
        // COMPONENT that could walk out of the state dir.
        assert!(!p
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir)));
    }

    #[test]
    fn an_absent_ledger_reads_as_empty_not_undetermined() {
        let dir = tempfile::tempdir().unwrap();
        let p = session_path(dir.path(), "s1");
        match load(&p) {
            Determination::Known(f) => assert_eq!(f.slots.len(), 0),
            Determination::Undetermined(why) => {
                panic!("absence must be Known(empty), got undetermined: {why:?}")
            }
        }
    }

    #[test]
    fn a_corrupt_ledger_is_undetermined_not_empty() {
        let dir = tempfile::tempdir().unwrap();
        let p = session_path(dir.path(), "s1");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, b"{not json").unwrap();
        assert!(
            matches!(load(&p), Determination::Undetermined(_)),
            "a corrupt store must not read as an empty one"
        );
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let p = session_path(dir.path(), "s1");
        let mut f = Inflight::default();
        let _ = f.acquire(SlotClass::Shell, "k", 7, 3);
        save(&p, &f).unwrap();
        match load(&p) {
            Determination::Known(back) => assert_eq!(back, f),
            Determination::Undetermined(why) => panic!("{why:?}"),
        }
    }

    #[test]
    fn save_reports_failure_instead_of_swallowing_it() {
        // The whole reason this is not `store::save_json`: an unwritable store
        // must reach the caller, because an unrecorded slot is a call running
        // outside the count.
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("sessions");
        std::fs::write(&blocker, b"i am a file, not a directory").unwrap();
        let p = session_path(dir.path(), "s1");
        assert!(save(&p, &Inflight::default()).is_err());
    }

    #[test]
    fn the_lock_is_exclusive_and_released_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let p = session_path(dir.path(), "s1");
        let first = lock(&p);
        assert!(matches!(first, Determination::Known(_)));
        drop(first);
        assert!(matches!(lock(&p), Determination::Known(_)));
    }

    #[test]
    fn ca_parallelguard_01_guard_drop_never_touches_a_different_holders_lock() {
        // CA-parallelguard-01: releasing a lock guard must never destroy
        // ANOTHER holder's lock, nor let a third acquirer in while that
        // holder is live. This is written only against the API surface
        // common to both the mtime-steal scheme and the kernel-advisory-lock
        // scheme (`lock`, `lock_path_for`, `session_path`, `Determination`,
        // std), so it compiles and is meaningful against either
        // implementation: whatever mechanism produced a *different*, live
        // holder's file at the same lock path, releasing an unrelated guard
        // must not remove it.
        let dir = tempfile::tempdir().unwrap();
        let p = session_path(dir.path(), "s1");

        let guard_a = match lock(&p) {
            Determination::Known(g) => g,
            Determination::Undetermined(why) => panic!("A failed to acquire: {why}"),
        };

        let lock_path = lock_path_for(&p);

        // Stand in for a second, independent, live holder that now occupies
        // the SAME lock path -- e.g. after a legitimate steal-and-recreate
        // elsewhere, or simply a second process's own lockfile landing at
        // the same path once the original file was replaced. However it got
        // there, releasing A must not know or care about it.
        std::fs::remove_file(&lock_path).unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)
            .unwrap();

        // A's own critical section finishes and it releases.
        drop(guard_a);

        assert!(
            lock_path.exists(),
            "releasing guard A deleted a lockfile that belongs to a \
             different, live holder just because it sat at the same path \
             -- a third caller could now acquire concurrently with that \
             holder"
        );
    }

    #[test]
    fn ca_parallelguard_02_concurrent_contenders_never_both_hold_an_abandoned_lock() {
        // CA-parallelguard-02: two contenders racing for the SAME abandoned
        // (genuinely dead, nobody-alive-at-the-other-end) lock must never
        // BOTH end up holding it at once -- no over-admission. Written only
        // against the common API surface (`lock`, `lock_path_for`,
        // `session_path`, `Determination`, std), with real concurrent
        // threads racing through the real `lock()` so the test exercises
        // whatever recovery mechanism the implementation actually uses
        // (mtime-steal-by-path, or kernel `try_lock`) rather than assuming
        // one. An atomic canary records how many guards were simultaneously
        // alive across the run; over-admission shows up as a canary value
        // above 1.
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::{Arc, Barrier};

        const THREADS: usize = 24;
        const ITERATIONS: usize = 40;

        for iteration in 0..ITERATIONS {
            let dir = tempfile::tempdir().unwrap();
            let p = session_path(dir.path(), "s1");
            let lock_path = lock_path_for(&p);
            std::fs::create_dir_all(lock_path.parent().unwrap()).unwrap();

            // A genuinely dead holder: a lockfile exists, nobody has it
            // open, and its mtime is far older than any staleness window a
            // recovery scheme might use.
            std::fs::write(&lock_path, b"").unwrap();
            let ancient = SystemTime::now() - Duration::from_secs(3600);
            std::fs::OpenOptions::new()
                .write(true)
                .open(&lock_path)
                .unwrap()
                .set_modified(ancient)
                .unwrap();

            let concurrent = Arc::new(AtomicUsize::new(0));
            let max_concurrent = Arc::new(AtomicUsize::new(0));
            let barrier = Arc::new(Barrier::new(THREADS));

            std::thread::scope(|scope| {
                for _ in 0..THREADS {
                    let p = p.clone();
                    let concurrent = Arc::clone(&concurrent);
                    let max_concurrent = Arc::clone(&max_concurrent);
                    let barrier = Arc::clone(&barrier);
                    scope.spawn(move || {
                        barrier.wait();
                        if let Determination::Known(guard) = lock(&p) {
                            let now = concurrent.fetch_add(1, Ordering::SeqCst) + 1;
                            max_concurrent.fetch_max(now, Ordering::SeqCst);
                            // Hold briefly so a genuinely concurrent second
                            // admission has a real window to be observed.
                            std::thread::sleep(Duration::from_millis(20));
                            concurrent.fetch_sub(1, Ordering::SeqCst);
                            drop(guard);
                        }
                    });
                }
            });

            let observed = max_concurrent.load(Ordering::SeqCst);
            assert!(
                observed <= 1,
                "over-admission on iteration {iteration}: {observed} threads \
                 held the lock for the same abandoned lockfile at the same \
                 time -- two contenders' abandoned-judgement raced and both \
                 ended up with a Known guard"
            );
        }
    }

    #[test]
    fn reset_removes_the_ledger_but_never_the_lockfile() {
        // The lockfile is persistent under the kernel-lock design: unlinking
        // it while a hook holds it would let the next `lock` lock a fresh
        // inode at the same path concurrently with that holder.
        let dir = tempfile::tempdir().unwrap();
        let p = session_path(dir.path(), "s1");
        save(&p, &Inflight::default()).unwrap();
        let lp = lock_path_for(&p);
        std::fs::write(&lp, b"").unwrap();
        reset(&p);
        assert!(!p.exists());
        assert!(lp.exists());
    }

    /// Set an env var for the duration of `f`. Tests in one binary share a
    /// process, so this restores the previous value.
    fn temp_env<F: FnOnce()>(key: &str, value: Option<&str>, f: F) {
        let prev = std::env::var(key).ok();
        match value {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
        f();
        match prev {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }
}
