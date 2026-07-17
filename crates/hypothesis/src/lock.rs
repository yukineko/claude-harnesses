//! File lock that serializes the hypothesis store's load->mutate->save cycle.
//!
//! Every CLI invocation that mutates the store does `Store::load()` (read
//! `hypotheses.toml`), mutate in memory, then an implicit `save()` at the end
//! of the call (temp file + rename — atomic, but only for the write itself).
//! Two concurrent sessions/processes doing this at once race: both load the
//! same snapshot, each mutates a different hypothesis (or field), and the
//! second `save` clobbers the first (last-writer-wins TOCTOU/lost update).
//! This module gives the whole load->mutate->save cycle mutual exclusion via
//! a lock file held for the lifetime of the [`Store`](crate::store::Store).
//!
//! Ported from the proven design in `backlog::lock` / `condukt::lock::RunLock`
//! / `overwatch::lock::LeaseLock`: the lock is published with a hard link
//! (`link(2)` fails `EEXIST` if the target already exists, so exactly one
//! racer wins the publish and a reader never observes a partial file), stale
//! locks whose owner pid is gone are reaped, and the reap/retry loop is
//! bounded. Like `condukt::lock::RunLock` (and unlike `backlog::lock`, which
//! fails fast) this lock *waits* (bounded) for a live holder to release, so
//! concurrent RMW cycles serialize and both complete rather than one
//! erroring out. It is fail-soft: if the lock cannot be acquired within the
//! deadline it degrades to proceeding unlocked (logged) rather than failing
//! the caller's load, and it never panics.

use crate::config::Config;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Process-wide monotonic counter so two threads in the SAME process (identical
/// pid, and possibly an identical `now_unix_nanos()` under a coarse clock) never
/// derive the same private temp-lock name. Without it a nanos collision makes
/// the loser's `create_new` fail `AlreadyExists`, degrading it to unlocked —
/// the exact race this lock exists to prevent.
static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Serialize, Deserialize)]
struct LockInfo {
    pid: u32,
    acquired_at: i64,
}

/// RAII guard for the store's file lock. Held across a load->mutate->save
/// cycle and released (best-effort) on drop. When `path` is `None` the lock
/// was not held (fail-soft degrade after a timeout) and drop is a no-op.
#[must_use = "the store lock is released as soon as this guard is dropped"]
pub struct StoreLock {
    path: Option<PathBuf>,
}

impl Drop for StoreLock {
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

fn read_info(path: &Path) -> Option<LockInfo> {
    let txt = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&txt).ok()
}

/// Lock file path — sits beside `hypotheses.toml` in the same store dir, so
/// unrelated stores (a test override via `--store-dir`-equivalent) never
/// share a lock.
fn lock_path(cfg: &Config) -> PathBuf {
    cfg.store_dir.join("hypotheses.lock")
}

impl StoreLock {
    /// Default bounded wait before degrading to unlocked. Generous enough
    /// that a normal RMW cycle (a few file ops) always releases well within
    /// it.
    const DEADLINE: Duration = Duration::from_secs(10);

    /// Acquire the store lock, waiting (bounded) for any live holder to
    /// release. Reaps a stale lock whose owner pid is gone. Never fails: on
    /// timeout it logs and returns an unlocked guard so the caller's load
    /// still proceeds (fail-soft).
    pub fn acquire(cfg: &Config) -> Self {
        Self::acquire_with_deadline(cfg, Self::DEADLINE)
    }

    /// Like [`StoreLock::acquire`] but with an explicit deadline (used by
    /// tests).
    pub fn acquire_with_deadline(cfg: &Config, deadline: Duration) -> Self {
        let path = lock_path(cfg);

        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                eprintln!(
                    "hypothesis: could not create lock dir {} ({e}); proceeding unlocked",
                    parent.display()
                );
                return StoreLock { path: None };
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
            Err(_) => return StoreLock { path: None },
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
                        eprintln!("hypothesis: could not write temp lock; proceeding unlocked");
                        return StoreLock { path: None };
                    }
                    f.sync_all().ok();
                }
                Err(e) => {
                    eprintln!("hypothesis: could not create temp lock ({e}); proceeding unlocked");
                    return StoreLock { path: None };
                }
            }
        }
        let _guard = TmpGuard(&tmp_path);

        let start = Instant::now();
        loop {
            match std::fs::hard_link(&tmp_path, &path) {
                Ok(()) => return StoreLock { path: Some(path) },
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
                                    "hypothesis: store lock contended for {:?}; \
                                     proceeding unlocked (update may race)",
                                    deadline
                                );
                                return StoreLock { path: None };
                            }
                            std::thread::sleep(Duration::from_millis(10));
                            continue;
                        }
                    }
                }
                Err(e) => {
                    eprintln!(
                        "hypothesis: could not publish lock {} ({e}); proceeding unlocked",
                        path.display()
                    );
                    return StoreLock { path: None };
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
