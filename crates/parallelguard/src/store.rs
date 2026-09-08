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

/// How long a lockfile may exist before it is treated as abandoned.
///
/// The critical section it guards is a read, a `Vec` push, and a rename —
/// microseconds. A lockfile older than this was left by a process that died
/// between `create_new` and `Drop` (SIGKILL, power loss); without stealing it,
/// one killed hook would deny every metered call for the rest of the session
/// with no way for the agent to recover — a gate only a human can clear, which
/// is a defect in the gate. Stealing cannot over-admit: the steal is itself
/// serialized by `create_new`, so exactly one thief wins and the losers retry.
const STALE_LOCK: Duration = Duration::from_secs(30);

/// Lock acquisition budget: 400 attempts x 5 ms = up to 2 s. Sized well above
/// the handful of concurrent hook processes one session can produce, so
/// exhausting it means something is genuinely wrong rather than merely busy.
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

/// RAII lock guard: the lockfile is removed on drop, including during a panic
/// unwind, so a crashing hook does not wedge the session for `STALE_LOCK`.
pub struct LockGuard {
    path: PathBuf,
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn lock_path_for(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".lock");
    PathBuf::from(s)
}

/// Whether a lockfile is old enough to be considered abandoned. An unreadable
/// or future-dated mtime is NOT stale: without a readable age there is no
/// evidence the holder is dead, and stealing on no evidence is the permissive
/// guess this module refuses to make.
fn lock_is_abandoned(lock_path: &Path, now: SystemTime) -> bool {
    let Ok(meta) = std::fs::metadata(lock_path) else {
        return false;
    };
    let Ok(modified) = meta.modified() else {
        return false;
    };
    match now.duration_since(modified) {
        Ok(age) => age > STALE_LOCK,
        Err(_) => false,
    }
}

/// Take the advisory lock guarding `path`'s critical section.
///
/// `create_new` is atomic at the filesystem level, so this serializes concurrent
/// hook processes without a new dependency. Failure to acquire is
/// `Undetermined`, never a silent "assume nothing is in flight".
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
    for _ in 0..LOCK_ATTEMPTS {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)
        {
            Ok(_) => return Determination::known(LockGuard { path: lock_path }),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                if lock_is_abandoned(&lock_path, SystemTime::now()) {
                    let _ = std::fs::remove_file(&lock_path);
                    continue;
                }
                std::thread::sleep(LOCK_DELAY);
            }
            Err(e) => {
                return Determination::undetermined(format!(
                    "cannot create the lockfile {}: {e}",
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

/// Drop a session's ledger and any lockfile left behind with it.
///
/// This is the turn boundary's self-heal: whatever leaked (a call the user
/// rejected, a hook killed mid-flight, a corrupt store) is gone by the next
/// turn without anyone having to intervene.
pub fn reset(path: &Path) {
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(lock_path_for(path));
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
    fn a_fresh_lockfile_is_not_abandoned() {
        let dir = tempfile::tempdir().unwrap();
        let lp = dir.path().join("x.lock");
        std::fs::write(&lp, b"").unwrap();
        assert!(!lock_is_abandoned(&lp, SystemTime::now()));
    }

    #[test]
    fn an_old_lockfile_is_abandoned() {
        let dir = tempfile::tempdir().unwrap();
        let lp = dir.path().join("x.lock");
        std::fs::write(&lp, b"").unwrap();
        let future = SystemTime::now() + STALE_LOCK + Duration::from_secs(5);
        assert!(lock_is_abandoned(&lp, future));
    }

    #[test]
    fn an_unreadable_lock_age_is_not_abandoned() {
        // No evidence the holder is dead => no steal. Absence of an mtime is
        // not evidence of death.
        let dir = tempfile::tempdir().unwrap();
        assert!(!lock_is_abandoned(
            &dir.path().join("nope.lock"),
            SystemTime::now()
        ));
    }

    #[test]
    fn reset_removes_the_ledger_and_a_leftover_lock() {
        let dir = tempfile::tempdir().unwrap();
        let p = session_path(dir.path(), "s1");
        save(&p, &Inflight::default()).unwrap();
        let lp = lock_path_for(&p);
        std::fs::write(&lp, b"").unwrap();
        reset(&p);
        assert!(!p.exists());
        assert!(!lp.exists());
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
