//! Per-run file lock that serializes run-state read-modify-write cycles.
//!
//! condukt's run state lives at `<state_dir>/<project-key>/<run-id>.json` and is
//! updated with a load→mutate→save cycle (`pause_run`, `resume_run`,
//! `StateAction::Set`). Two concurrent sessions/worktrees doing this at once
//! race: both load the same snapshot, each mutates a different field, and the
//! second `save` clobbers the first (last-writer-wins TOCTOU). This module gives
//! each run a lock file next to its state so the whole load→mutate→save cycle is
//! mutually exclusive per run — unrelated runs never block each other.
//!
//! Reuses the proven atomicity from `backlog::lock`: the lock is published with a
//! hard link (link(2) fails `EEXIST` if the target already exists, so exactly one
//! racer wins the publish and a reader never observes a partial file), stale
//! locks whose owner pid is gone are reaped, and the reap/retry loop is bounded.
//! Unlike `backlog::lock` — which fails fast when a live holder exists — this lock
//! *waits* (bounded) for the holder to release so concurrent RMW cycles serialize
//! and both complete. It is fail-soft: if the lock cannot be acquired within the
//! deadline it degrades to proceeding unlocked (logged) rather than failing the
//! caller's state update, and it never panics.

use crate::config::Config;
use crate::store::{project_key, repo_root};
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
    run_id: String,
    acquired_at: i64,
}

/// RAII guard for a per-run state lock. Held across a load→mutate→save cycle and
/// released (best-effort) on drop. When `path` is `None` the lock was not held
/// (fail-soft degrade after a timeout) and drop is a no-op.
#[must_use = "the run lock is released as soon as this guard is dropped"]
pub struct RunLock {
    path: Option<PathBuf>,
}

impl Drop for RunLock {
    fn drop(&mut self) {
        if let Some(p) = &self.path {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// Reserved run-id used only to key the REPO-scoped primary lock file
/// (`<project>/__repo_primary__.lock`). A FIXED key (independent of any run_id)
/// yields ONE per-repo/project lock, so every condukt process that mutates the
/// single primary repo's default branch — `worktree::merge` (checkout
/// default_branch + merge), the main-tree selective-staging commit, and
/// `git worktree prune` — serializes on it instead of racing on `main` (which
/// today is only serialized by the upstream flow backlog lock). Mirrors
/// `claim::CLAIMS_LOCK_KEY`; it never names a real run so cannot collide with one.
pub const REPO_PRIMARY_LOCK_KEY: &str = "__repo_primary__";

/// Acquire the repo-scoped primary lock for the repo containing `cwd`, holding
/// it (via the returned RAII guard) for the whole primary-repo critical section.
/// Fail-soft: never panics, and under pathological contention may degrade to
/// unlocked exactly like [`RunLock::acquire`] (bounded wait, pid-reaped).
pub fn acquire_repo_primary(cfg: &Config, cwd: &Path) -> RunLock {
    RunLock::acquire(cfg, cwd, REPO_PRIMARY_LOCK_KEY)
}

/// Like [`acquire_repo_primary`] but loads [`Config`] internally, for
/// primary-repo mutators (e.g. `git worktree prune` inside
/// `worktree::create`/`worktree::discard`) that do not already thread a `Config`
/// and whose callers live in sibling modules. Same fail-soft guarantees.
pub fn acquire_repo_primary_loaded(cwd: &Path) -> RunLock {
    acquire_repo_primary(&Config::load(), cwd)
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

/// Lock file path for a run — sits beside the run's `<run-id>.json` state file,
/// keyed the same way (sanitised run id, per project) so unrelated runs and
/// unrelated projects never share a lock.
fn lock_path(cfg: &Config, cwd: &Path, run_id: &str) -> PathBuf {
    let dir = cfg.state_dir.join(project_key(&repo_root(cwd)));
    dir.join(format!(
        "{}.lock",
        harness_core::store::safe_session(run_id)
    ))
}

fn read_info(path: &Path) -> Option<LockInfo> {
    let txt = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&txt).ok()
}

impl RunLock {
    /// Default bounded wait before degrading to unlocked. Generous enough that a
    /// normal RMW cycle (a few file ops) always releases well within it.
    const DEADLINE: Duration = Duration::from_secs(10);

    /// Acquire the per-run lock, waiting (bounded) for any live holder to
    /// release. Reaps a stale lock whose owner pid is gone. Never fails: on
    /// timeout it logs and returns an unlocked guard so the caller's state
    /// update still proceeds (fail-soft).
    pub fn acquire(cfg: &Config, cwd: &Path, run_id: &str) -> Self {
        Self::acquire_with_deadline(cfg, cwd, run_id, Self::DEADLINE)
    }

    /// Like [`RunLock::acquire`] but with an explicit deadline (used by tests).
    pub fn acquire_with_deadline(
        cfg: &Config,
        cwd: &Path,
        run_id: &str,
        deadline: Duration,
    ) -> Self {
        Self::acquire_at(lock_path(cfg, cwd, run_id), deadline)
    }

    /// Returns `true` when this guard genuinely holds the lock. A `false` here
    /// means the lock degraded to unlocked (fail-soft after a timeout or I/O
    /// error) and any RMW performed under it may race — the caller should treat
    /// that as unsafe to mutate.
    // `#[allow(dead_code)]`: this is a new caller-facing API; its callers live in
    // sibling modules (state.rs/claim.rs) owned by other tasks and are wired
    // separately. condukt is a bin crate, so an as-yet-uncalled `pub fn` reads
    // as dead until then. Exercised now by this module's tests.
    #[allow(dead_code)]
    pub fn held(&self) -> bool {
        self.path.is_some()
    }

    /// Fallible acquire: returns `Some(guard)` only when the lock is genuinely
    /// HELD, and `None` when acquisition degraded to unlocked (timeout under
    /// contention, or an I/O error) — a treat-as-held **hard-skip**. Lets a
    /// caller SKIP its read-modify-write instead of mutating unlocked, which
    /// under pathological contention is what lets two timed-out writers both
    /// proceed and double-write (last-writer-wins). Fail-soft: never panics.
    /// Uses the same [`RunLock::DEADLINE`] as [`RunLock::acquire`].
    #[allow(dead_code)] // caller-facing API wired by sibling tasks; see `held`.
    pub fn acquire_or_skip(cfg: &Config, cwd: &Path, run_id: &str) -> Option<Self> {
        Self::acquire_or_skip_at(lock_path(cfg, cwd, run_id), Self::DEADLINE)
    }

    /// Core of [`RunLock::acquire_or_skip`] against an explicit lock `path`:
    /// runs the same fail-soft acquire and maps the degraded (unheld) guard to
    /// `None`. Shared by the public API and the seam tests (which drive it with
    /// a short deadline against a self-contained temp path).
    #[allow(dead_code)] // reached via `acquire_or_skip` (wired by sibling tasks) + tests.
    fn acquire_or_skip_at(path: PathBuf, deadline: Duration) -> Option<Self> {
        let guard = Self::acquire_at(path, deadline);
        if guard.held() {
            Some(guard)
        } else {
            None
        }
    }

    /// Core locking mechanics against an explicit lock-file `path`. Split out
    /// from [`RunLock::acquire_with_deadline`] so both the fallible
    /// `acquire_or_skip` and the tests can drive it against a self-contained
    /// temp path. Stays fail-soft: returns `RunLock { path: None }` on any
    /// timeout/error so existing callers of `acquire`/`acquire_with_deadline`
    /// are unchanged.
    fn acquire_at(path: PathBuf, deadline: Duration) -> Self {
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                eprintln!(
                    "condukt: could not create lock dir {} ({e}); proceeding unlocked",
                    parent.display()
                );
                return RunLock { path: None };
            }
        }

        // Fully write our lock contents to a private temp file first, then
        // publish it atomically via hard link. A concurrent reader can never
        // observe a partial lock at the final path. The run id is recorded for
        // diagnostics only (never read functionally — reap keys off `pid`); it
        // is recovered from the lock filename (`<safe_session(run_id)>.lock`)
        // since `acquire_at` operates on an already-resolved path.
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
        let json = match serde_json::to_string(&info) {
            Ok(j) => j,
            Err(_) => return RunLock { path: None },
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
                        eprintln!("condukt: could not write temp lock; proceeding unlocked");
                        return RunLock { path: None };
                    }
                    f.sync_all().ok();
                }
                Err(e) => {
                    eprintln!("condukt: could not create temp lock ({e}); proceeding unlocked");
                    return RunLock { path: None };
                }
            }
        }
        let _guard = TmpGuard(&tmp_path);

        let start = Instant::now();
        loop {
            match std::fs::hard_link(&tmp_path, &path) {
                Ok(()) => return RunLock { path: Some(path) },
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
                                    "condukt: state lock {} contended for {:?}; \
                                     proceeding unlocked (update may race)",
                                    path.display(),
                                    deadline
                                );
                                return RunLock { path: None };
                            }
                            std::thread::sleep(Duration::from_millis(10));
                            continue;
                        }
                    }
                }
                Err(e) => {
                    eprintln!(
                        "condukt: could not publish lock {} ({e}); proceeding unlocked",
                        path.display()
                    );
                    return RunLock { path: None };
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
    use std::sync::atomic::{AtomicU32, Ordering as AtomicOrdering};

    // Drives `acquire_at`/`acquire_or_skip_at` directly against a self-contained
    // temp path (never `lock_path`'s state-dir resolution), so these tests never
    // touch a real `state_dir` and need no coordination with other tests.
    fn tmp_lock_path(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "condukt-lock-test-{tag}-{}-{}",
            std::process::id(),
            now_unix_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("run.lock")
    }

    #[test]
    fn held_reflects_acquisition_state() {
        let path = tmp_lock_path("held");
        let g = RunLock::acquire_at(path, Duration::from_millis(200));
        assert!(g.held(), "a freshly-published lock must report held()");
    }

    // Wedge a holder, then attempt `acquire_or_skip` against the SAME lock path
    // with a short deadline: it must hard-skip (`None`) rather than hand out an
    // unheld guard. A guarded RMW modeled as a closure gated on `Some` must run
    // exactly once (the holder), NOT twice — this is the window that a plain
    // `acquire` degrade would let a second writer through, double-writing.
    #[test]
    fn acquire_or_skip_hard_skips_while_first_is_held() {
        let path = tmp_lock_path("skip");
        let writes = AtomicU32::new(0);
        // A guarded RMW: it only mutates when handed a genuinely-held guard.
        let guarded_rmw = |lock: Option<RunLock>| {
            if let Some(g) = lock {
                assert!(g.held());
                writes.fetch_add(1, AtomicOrdering::Relaxed);
            }
        };

        // First writer genuinely holds the lock and performs its RMW. Keep the
        // guard alive across the second writer's attempt to model overlap.
        let holder = RunLock::acquire_or_skip_at(path.clone(), Duration::from_millis(200));
        assert!(holder.is_some(), "first acquire_or_skip must be HELD");
        // Second writer contends the SAME lock with a short deadline: it must
        // hard-skip (None), NOT hand out an unheld guard that proceeds unlocked.
        let contended = RunLock::acquire_or_skip_at(path, Duration::from_millis(80));
        assert!(
            contended.is_none(),
            "contended acquire_or_skip must hard-skip (None), not proceed unlocked"
        );

        guarded_rmw(holder); // runs once (genuine holder)
        guarded_rmw(contended); // None -> skipped, no second write

        // Exactly one RMW ran; the contended writer skipped instead of
        // double-writing (last-writer-wins).
        assert_eq!(writes.load(AtomicOrdering::Relaxed), 1);
    }

    fn test_cfg(state_dir: PathBuf) -> Config {
        Config {
            worktree_base: state_dir.join("worktrees"),
            default_branch: "main".to_string(),
            shared_globs: Vec::new(),
            max_parallel: 4,
            state_dir,
            test_command: None,
            stuck_ttl_secs: 1800,
            build_command: None,
            deploy_command: None,
            loop_max_iters: 10,
            autonomous: false,
            consensus_enabled: false,
            consensus_samples: crate::consensus::DEFAULT_SAMPLES,
            consensus_threshold: crate::consensus::DEFAULT_THRESHOLD,
            adversarial_enabled: false,
            adversarial_size: crate::adversarial::DEFAULT_PANEL,
            adversarial_min_voters: crate::adversarial::DEFAULT_MIN_VOTERS,
            adversarial_block_ratio: crate::adversarial::DEFAULT_BLOCK_RATIO,
            single_worktree: false,
            worker_sandbox_enabled: false,
            worker_sandbox_image: None,
            worker_sandbox_memory: None,
            worker_sandbox_cpus: None,
            worker_sandbox_pids_limit: None,
        }
    }

    // The core serialization proof for the repo-scoped primary lock: many
    // threads each do a read-modify-write on ONE shared counter file, every RMW
    // guarded by the SAME repo-primary lock path. A widened window
    // (read -> sleep -> write) makes an UNLOCKED racer almost certainly clobber
    // a concurrent increment (lost update) — this is the exact bug two condukt
    // runs racing on `main` would hit. Under the shared lock every increment
    // must survive: final == THREADS*ITERS. RED without the guard (drop `_g` and
    // the counter under-counts), GREEN with it.
    #[test]
    fn repo_primary_lock_serializes_concurrent_rmw_no_lost_update() {
        let dir = std::env::temp_dir().join(format!(
            "condukt-repo-primary-rmw-{}-{}",
            std::process::id(),
            now_unix_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        // A FIXED lock path shared by every thread models the repo-scoped key
        // (`REPO_PRIMARY_LOCK_KEY`): one lock for the one primary repo.
        let lock_file = dir.join(format!("{REPO_PRIMARY_LOCK_KEY}.lock"));
        let counter = dir.join("counter");
        std::fs::write(&counter, "0").unwrap();

        const THREADS: usize = 6;
        const ITERS: usize = 8;

        std::thread::scope(|scope| {
            for _ in 0..THREADS {
                let lock_file = lock_file.clone();
                let counter = counter.clone();
                scope.spawn(move || {
                    for _ in 0..ITERS {
                        let g = RunLock::acquire_at(lock_file.clone(), Duration::from_secs(10));
                        assert!(g.held(), "each RMW must genuinely hold the repo lock");
                        // Widened read->modify->write window: an unlocked racer
                        // reading the same `cur` here would lose an increment.
                        let cur: u64 = std::fs::read_to_string(&counter)
                            .unwrap()
                            .trim()
                            .parse()
                            .unwrap();
                        std::thread::sleep(Duration::from_millis(2));
                        std::fs::write(&counter, (cur + 1).to_string()).unwrap();
                    }
                });
            }
        });

        let final_val: u64 = std::fs::read_to_string(&counter)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(
            final_val,
            (THREADS * ITERS) as u64,
            "every increment must survive under the repo-primary lock (no lost update)"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    // The repo-primary lock is REPO-scoped, not run-scoped: `acquire_repo_primary`
    // keys off the FIXED reserved id, so its lock file lands at
    // `<state_dir>/<project-key>/__repo_primary__.lock` regardless of any run id
    // — the single shared path every primary-repo mutator (merge / main-tree
    // commit / prune) contends. It is genuinely HELD and distinct from the
    // claims registry lock.
    #[test]
    fn acquire_repo_primary_is_held_and_repo_scoped() {
        assert_eq!(REPO_PRIMARY_LOCK_KEY, "__repo_primary__");

        let base = std::env::temp_dir().join(format!(
            "condukt-repo-primary-scope-{}-{}",
            std::process::id(),
            now_unix_nanos()
        ));
        let cwd = base.join("repo");
        std::fs::create_dir_all(&cwd).unwrap();
        let cfg = test_cfg(base.join("state"));

        let expected = cfg
            .state_dir
            .join(project_key(&repo_root(&cwd)))
            .join(format!(
                "{}.lock",
                harness_core::store::safe_session(REPO_PRIMARY_LOCK_KEY)
            ));

        let guard = acquire_repo_primary(&cfg, &cwd);
        assert!(guard.held(), "repo-primary lock must be genuinely held");
        assert!(
            expected.exists(),
            "repo-primary lock file must be published at the repo-scoped path {}",
            expected.display()
        );
        drop(guard);
        assert!(
            !expected.exists(),
            "lock file must be released (removed) on guard drop"
        );
        std::fs::remove_dir_all(&base).ok();
    }
}
