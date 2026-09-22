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
//! and both complete. The bounded wait IS the retry: a live holder is waited out
//! for [`RunLock::DEADLINE`], and a dead holder's lock is reaped immediately.
//!
//! **Cannot-acquire resolves to the RESTRICTIVE side.** There is no public entry
//! point that hands back an unheld guard: every acquisition is fallible
//! ([`RunLock::acquire_or_skip`] → `Option`, [`acquire_repo_primary`] →
//! `Result`), so "the deadline expired / an I/O error stopped me" cannot be
//! silently rendered as "nobody else holds this". It never panics: the failure
//! is a `None`/`Err` the caller must handle, not an abort.

use anyhow::{bail, Result};

use crate::config::Config;
use crate::store::{project_key, repo_root};
use harness_core::verdict::{Determination, Required};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Process-wide monotonic counter so two threads in the SAME process (identical
/// pid, and possibly an identical `now_unix_nanos()` under a coarse clock) never
/// derive the same private temp-lock name. Without it a nanos collision makes the
/// loser's `create_new` fail `AlreadyExists`, turning a perfectly acquirable lock
/// into a spurious acquisition failure (now a refusal, previously an unlocked
/// proceed — the exact race this lock exists to prevent).
static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Serialize, Deserialize)]
struct LockInfo {
    pid: u32,
    run_id: String,
    acquired_at: i64,
}

/// RAII guard for a per-run state lock. Held across a load→mutate→save cycle and
/// released (best-effort) on drop. `path == None` is the INTERNAL representation
/// of a failed acquisition; it never escapes this module as a usable guard —
/// [`RunLock::acquire_or_skip`] maps it to `None` and [`acquire_repo_primary`]
/// maps it to `Err`, so a caller can never be handed an unheld guard.
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
///
/// The fixed key alone is not sufficient for that "ONE per-repo lock": the
/// PROJECT it is keyed under must also be repo-wide, which is why
/// [`acquire_repo_primary`] resolves its project root through
/// [`harness_core::projkey::main_worktree_root`] and not `repo_root`.
pub const REPO_PRIMARY_LOCK_KEY: &str = "__repo_primary__";

/// The project root the repo-primary lock is keyed by: the MAIN worktree root
/// of the repo containing `cwd`, so every linked worktree of that repo and the
/// main tree itself address ONE lock file.
///
/// Why not `repo_root` (which every run-state path still uses): `repo_root`
/// stops at the first ancestor holding a `.git` ENTRY, and in a linked worktree
/// `.git` is a FILE — so it returns the LINKED WORKTREE, giving each checkout
/// its own project key and therefore its own `__repo_primary__.lock`. Two
/// condukt processes would then each hold "the" primary lock and both mutate
/// the ONE thing this lock protects: the shared git index, the shared
/// `.git/worktrees` admin dir and the shared default branch. Under CLAUDE.md §8
/// every session works in its own linked worktree, so that split is the normal
/// case, not an edge one.
///
/// Run state is deliberately NOT re-keyed here: it is per-run bookkeeping that
/// already lives on disk under `repo_root`-derived keys, and moving it would
/// orphan those files (backlog `43393ce2`). Same opt-in shape the claim
/// registry took in `d83b0e8f`.
///
/// `Undetermined` is a legitimate answer and is never collapsed into a path —
/// see [`harness_core::projkey::main_worktree_root`] for the arms.
fn repo_primary_project_root(cwd: &Path) -> Determination<PathBuf> {
    harness_core::projkey::main_worktree_root(cwd)
}

/// Acquire the repo-scoped primary lock for the repo containing `cwd`, holding
/// it (via the returned RAII guard) for the whole primary-repo critical section.
///
/// The lock file is `<state_dir>/<project-key of the MAIN WORKTREE ROOT>/`
/// `__repo_primary__.lock` (see [`repo_primary_project_root`]), so a caller
/// standing in a linked worktree and a caller standing in the main tree of the
/// SAME repo contend that one file rather than each taking a private lock.
///
/// **Fallible on purpose, in two ways.**
///
/// 1. The bounded wait already absorbs ordinary contention (a live holder is
///    waited out for [`RunLock::DEADLINE`], a dead holder is reaped and retried
///    immediately), so reaching the deadline — or hitting an I/O/serialization
///    error — means we genuinely cannot determine whether a peer is
///    mid-mutation of the one primary repo.
/// 2. The project root itself may be `Undetermined` (an unreadable/unparseable
///    `.git` gitfile, a missing `commondir`, a candidate root that cannot be
///    verified). Then we do not know WHICH lock file is the repo's one lock,
///    and there is no fallback: falling back to `repo_root(cwd)` would publish
///    a per-checkout lock that a main-tree peer never contends — i.e. it would
///    recreate exactly the split this keying removes, while looking locked.
///
/// Both are cannot-determine, and both resolve to the restrictive side: `Err`,
/// so the caller refuses instead of mutating `main`/the shared index/the
/// worktree admin dir unlocked. Never panics.
pub fn acquire_repo_primary(cfg: &Config, cwd: &Path) -> Result<RunLock> {
    let root = match repo_primary_project_root(cwd).require() {
        Required::Determined(root) => root,
        Required::Blocked(v) => {
            let why = match v.reason() {
                Some(r) => r.as_str().to_string(),
                None => "no reason recorded".to_string(),
            };
            bail!(
                "could not determine the main worktree root for {} ({why}); \
                 refusing to mutate the primary repo unlocked, because the one \
                 lock file every checkout of this repo must contend cannot be \
                 addressed (falling back to a per-checkout path would publish a \
                 lock no peer contends)",
                cwd.display()
            );
        }
    };
    match RunLock::acquire_or_skip_in_project(cfg, &root, REPO_PRIMARY_LOCK_KEY) {
        Some(guard) => Ok(guard),
        None => bail!(
            "could not acquire the repo-primary lock for {} (main worktree root {}) \
             within {:?}; refusing to mutate the primary repo unlocked (a concurrent \
             condukt execution could be merging, committing into the shared index, or \
             pruning worktrees at the same time)",
            cwd.display(),
            root.display(),
            RunLock::DEADLINE
        ),
    }
}

/// Like [`acquire_repo_primary`] but loads [`Config`] internally, for
/// primary-repo mutators (`worktree::create`, `worktree::discard`) that do not
/// already thread a `Config` and whose callers live in sibling modules. Same
/// fallible contract: cannot-acquire is `Err`, never an unheld guard.
pub fn acquire_repo_primary_loaded(cwd: &Path) -> Result<RunLock> {
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

/// Lock file path for a run — sits beside the run's `<run-id>.json` state file,
/// keyed the same way (sanitised run id, per project) so unrelated runs and
/// unrelated projects never share a lock.
fn lock_path(cfg: &Config, cwd: &Path, run_id: &str) -> PathBuf {
    lock_path_in_project(cfg, &repo_root(cwd), run_id)
}

/// Lock file path for a run under an ALREADY-RESOLVED project root.
///
/// [`lock_path`] is this function with `repo_root(cwd)` as the root — the
/// composition is shared so the two can never drift in how they key the
/// directory or sanitise the run id. It exists separately because a caller
/// whose store is keyed by something other than `repo_root` (today: the claim
/// registry, which is keyed by the MAIN worktree root so every linked worktree
/// of a repo shares ONE registry) must put its lock beside its own store. If it
/// went through `lock_path` instead, the shared `claims.json` would be
/// read-modify-written under two different lock files — an unserialized race
/// introduced by sharing the file.
///
/// The repo-primary lock ([`acquire_repo_primary`]) is the second such caller,
/// for the same reason in the other direction: it protects a resource — the ONE
/// git index / `.git/worktrees` admin dir / default branch — that every linked
/// worktree shares, so it too must be keyed by the main worktree root.
pub(crate) fn lock_path_in_project(cfg: &Config, project_root: &Path, run_id: &str) -> PathBuf {
    let dir = cfg.state_dir.join(project_key(project_root));
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
    /// Default bounded wait before the acquisition FAILS (`None`/`Err` — never
    /// an unlocked proceed). Generous enough that a normal RMW cycle (a few file
    /// ops) always releases well within it.
    pub(crate) const DEADLINE: Duration = Duration::from_secs(10);

    /// Returns `true` when this guard genuinely holds the lock. Internal to the
    /// fallible acquire paths: a `false` here means acquisition failed (timeout
    /// or I/O error), which they map to `None`/`Err` rather than handing the
    /// guard out. There is deliberately no public `acquire` that returns an
    /// unheld guard — "I could not determine whether a peer holds this" must not
    /// be representable as a usable lock.
    pub fn held(&self) -> bool {
        self.path.is_some()
    }

    /// Fallible acquire: returns `Some(guard)` only when the lock is genuinely
    /// HELD, and `None` when acquisition degraded to unlocked (timeout under
    /// contention, or an I/O error) — a treat-as-held **hard-skip**. Lets a
    /// caller SKIP its read-modify-write instead of mutating unlocked, which
    /// under pathological contention is what lets two timed-out writers both
    /// proceed and double-write (last-writer-wins). Never panics.
    /// Waits up to [`RunLock::DEADLINE`]. Live callers:
    /// `state::with_run_locked`, `state::discard_experiment`, the `state set`
    /// CLI arm, `claim::{claim_tasks, release_*, heartbeat, active_claims,
    /// write_execution_state}`, `repo_commit::commit`, and (via
    /// [`acquire_repo_primary`]) every primary-repo mutator in `worktree`/`main`.
    pub fn acquire_or_skip(cfg: &Config, cwd: &Path, run_id: &str) -> Option<Self> {
        Self::acquire_or_skip_at(lock_path(cfg, cwd, run_id), Self::DEADLINE)
    }

    /// [`RunLock::acquire_or_skip`] against an ALREADY-RESOLVED project root
    /// instead of a `cwd`.
    ///
    /// Two callers use this, both because the thing they guard is shared by
    /// every checkout of a repo and so must be keyed by the MAIN worktree root:
    ///
    /// - the claim registry, for its reserved [`crate::claim::CLAIMS_LOCK_KEY`]
    ///   — every linked worktree read-modify-writes ONE `claims.json` and must
    ///   therefore contend ONE `__claims__.lock`;
    /// - [`acquire_repo_primary`], for [`REPO_PRIMARY_LOCK_KEY`] — every
    ///   checkout mutates ONE git index, ONE `.git/worktrees` admin dir and ONE
    ///   default branch, so they must contend ONE `__repo_primary__.lock`.
    ///
    /// Run state is NOT keyed that way, so every run-state caller stays on
    /// [`RunLock::acquire_or_skip`] and its `repo_root`-derived path is
    /// unchanged. Same fallible contract: a failed acquisition is `None`, never
    /// a usable guard.
    pub(crate) fn acquire_or_skip_in_project(
        cfg: &Config,
        project_root: &Path,
        run_id: &str,
    ) -> Option<Self> {
        Self::acquire_or_skip_at(
            lock_path_in_project(cfg, project_root, run_id),
            Self::DEADLINE,
        )
    }

    /// Deadline-parameterized [`RunLock::acquire_or_skip_in_project`], the
    /// project-root sibling of [`RunLock::acquire_or_skip_with_deadline`]. The
    /// hard-skip claims path ([`crate::claim::claim_files`]) delegates here so
    /// production (the 10s default) and its wedged-holder regression test (a
    /// short deadline) drive the SAME skip-on-contention code.
    pub(crate) fn acquire_or_skip_in_project_with_deadline(
        cfg: &Config,
        project_root: &Path,
        run_id: &str,
        deadline: Duration,
    ) -> Option<Self> {
        Self::acquire_or_skip_at(lock_path_in_project(cfg, project_root, run_id), deadline)
    }

    /// Core of [`RunLock::acquire_or_skip`] against an explicit lock `path`:
    /// runs the core acquire and maps the unheld guard to `None` so it cannot
    /// escape. Shared by the public API and the seam tests (which drive it with
    /// a short deadline against a self-contained temp path).
    fn acquire_or_skip_at(path: PathBuf, deadline: Duration) -> Option<Self> {
        let guard = Self::acquire_at(path, deadline);
        if guard.held() {
            Some(guard)
        } else {
            None
        }
    }

    /// Core locking mechanics against an explicit lock-file `path`. Private, so
    /// the unheld (`path: None`) result it returns on any timeout/error can only
    /// reach a caller through a fallible wrapper that turns it into `None`/`Err`
    /// — the tests drive it directly against a self-contained temp path.
    fn acquire_at(path: PathBuf, deadline: Duration) -> Self {
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                eprintln!(
                    "condukt: could not create lock dir {} ({e}); lock NOT acquired",
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
                        eprintln!("condukt: could not write temp lock; lock NOT acquired");
                        return RunLock { path: None };
                    }
                    f.sync_all().ok();
                }
                Err(e) => {
                    eprintln!("condukt: could not create temp lock ({e}); lock NOT acquired");
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
                                    "condukt: state lock {} contended for {:?}; \
                                     lock NOT acquired",
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
                        "condukt: could not publish lock {} ({e}); lock NOT acquired",
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
            autonomy_source: harness_core::autonomy::Source::BuiltinDefault,
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

    // The repo-primary lock is MAIN-WORKTREE-ROOT-scoped, not run-scoped:
    // `acquire_repo_primary` keys off the FIXED reserved id under the project key
    // of the repo's MAIN worktree root, so its lock file lands at
    // `<state_dir>/<project-key of the main worktree root>/__repo_primary__.lock`
    // regardless of any run id — the single shared path every primary-repo
    // mutator (merge / main-tree commit / prune) contends, from whichever
    // checkout it happens to stand in. It is genuinely HELD and distinct from
    // the claims registry lock.
    //
    // Re-anchored (was `project_key(&repo_root(&cwd))`, and its message called
    // the path "repo-scoped"): that prose named worktree-scoping as though it
    // were repo-scoping. The property this test fixes — ONE fixed reserved key
    // per repo, distinct from the claims-registry lock — is unchanged; only the
    // root resolution it pins is sharpened. That a LINKED worktree resolves to
    // the same root is the separate subject of
    // `repo_primary_lock_is_one_lock_across_a_repo_and_its_linked_worktree`.
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

        // Fixture invariant, observed directly rather than by re-deriving the
        // expectation from the code under test: `cwd` carries no `.git` entry
        // and no ancestor of a fresh temp dir does either, so there is no
        // linked-worktree indirection to follow and `cwd` IS its own main
        // worktree root. The expected key below is therefore anchored on the
        // literal `cwd`, not on a call to `repo_root`/`main_worktree_root`.
        assert!(
            !cwd.join(".git").exists(),
            "fixture invariant: {} must have no .git entry, so it is its own \
             main worktree root",
            cwd.join(".git").display()
        );
        let expected = cfg.state_dir.join(project_key(&cwd)).join(format!(
            "{}.lock",
            harness_core::store::safe_session(REPO_PRIMARY_LOCK_KEY)
        ));

        let guard = acquire_repo_primary(&cfg, &cwd).expect("repo-primary lock must be acquirable");
        assert!(guard.held(), "repo-primary lock must be genuinely held");
        assert!(
            expected.exists(),
            "repo-primary lock file must be published under the project key of the \
             repo's MAIN WORKTREE ROOT (not of whichever checkout the caller stands \
             in), at {}",
            expected.display()
        );
        drop(guard);
        assert!(
            !expected.exists(),
            "lock file must be released (removed) on guard drop"
        );
        std::fs::remove_dir_all(&base).ok();
    }

    // The type-level half of the five call-site regressions in `worktree.rs` /
    // `main.rs`: `acquire_repo_primary` itself must resolve cannot-acquire to
    // `Err`, never to an unheld guard. Injected failure: a `state_dir` that is a
    // regular FILE, so the lock dir can never be created.
    #[test]
    fn acquire_repo_primary_refuses_when_it_cannot_acquire() {
        let base = std::env::temp_dir().join(format!(
            "condukt-repo-primary-refuse-{}-{}",
            std::process::id(),
            now_unix_nanos()
        ));
        let cwd = base.join("repo");
        std::fs::create_dir_all(&cwd).unwrap();
        let state = base.join("state-is-a-file");
        std::fs::write(&state, b"not a directory\n").unwrap();

        // `match`, not `expect_err`: `RunLock` is intentionally not `Debug`.
        let msg = match acquire_repo_primary(&test_cfg(state), &cwd) {
            Ok(_) => panic!("an unacquirable repo-primary lock must be Err, not an unheld guard"),
            Err(e) => format!("{e:#}"),
        };
        assert!(
            msg.contains("repo-primary lock") && msg.contains("refusing"),
            "the error must say the lock could not be taken and that we refuse; got: {msg}"
        );
        std::fs::remove_dir_all(&base).ok();
    }

    // The `Undetermined` half of the main-worktree-root keying: when the repo's
    // main worktree root CANNOT be resolved, `acquire_repo_primary` must resolve
    // to `Err` and must publish NOTHING — never fall back to `repo_root(cwd)` or
    // any other path.
    //
    // Injected fault: a `.git` FILE with no `gitdir:` line, which is exactly the
    // `main-worktree-root: gitfile-unparseable` arm. Note the fault is chosen so
    // that a fallback would SUCCEED (`repo_root` stops right here, because `.git`
    // exists), which is what makes the two assertions able to tell a refusal from
    // a fallback: a fallback would leave a published `__repo_primary__.lock`
    // under the state dir that no main-tree peer ever contends — locked-looking
    // and unserialized, the very split this keying removes.
    //
    // NOTE ON PROVENANCE (CLAUDE.md §2(a)): this test was written by the same
    // agent that wrote the `Required::Blocked` arm it exercises, so it is WEAK
    // evidence and wants review by a disinterested party. Its kill power was
    // nonetheless observed, not assumed: with the arm mutated to
    // `Required::Blocked(_) => crate::store::repo_root(cwd)` (the fail-open) it
    // FAILS, at the first assertion — the grant is reached, so the run stops
    // there and the published-lock assertion below is NOT separately exercised
    // by that mutant.
    #[test]
    fn acquire_repo_primary_refuses_when_the_main_worktree_root_is_undetermined() {
        let base = std::env::temp_dir().join(format!(
            "condukt-repo-primary-undet-root-{}-{}",
            std::process::id(),
            now_unix_nanos()
        ));
        let cwd = base.join("repo");
        std::fs::create_dir_all(&cwd).unwrap();
        // A `.git` FILE that is not a parseable gitdir pointer.
        std::fs::write(cwd.join(".git"), b"this is not a gitdir pointer\n").unwrap();
        assert!(
            cwd.join(".git").is_file(),
            "fixture invariant: .git must be a FILE for the gitfile arm to be reached"
        );
        let cfg = test_cfg(base.join("state"));

        // `match`, not `expect_err`: `RunLock` is intentionally not `Debug`.
        let msg = match acquire_repo_primary(&cfg, &cwd) {
            Ok(_) => panic!(
                "an UNDETERMINED main worktree root must be `Err`: the one lock file \
                 every checkout of this repo must contend cannot be addressed, so \
                 there is no lock to hand out. Granting one here means a fallback \
                 path was used."
            ),
            Err(e) => format!("{e:#}"),
        };
        assert!(
            msg.contains("main worktree root") && msg.contains("refusing"),
            "the error must name the unresolved main worktree root and say we \
             refuse; got: {msg}"
        );
        assert!(
            published_repo_primary_locks(&cfg.state_dir).is_empty(),
            "an UNDETERMINED main worktree root must publish NO lock file at all; a \
             fallback key (e.g. `repo_root(cwd)`, which stops at this very dir) would \
             publish a per-checkout lock that no peer contends. Found {:?} under {}",
            published_repo_primary_locks(&cfg.state_dir),
            cfg.state_dir.display()
        );
        std::fs::remove_dir_all(&base).ok();
    }

    /// Run `git` in `dir`, failing the test loudly on a non-zero exit (a
    /// half-built fixture must never be mistaken for the property under test).
    fn git(dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap_or_else(|e| panic!("fixture: could not spawn `git {args:?}`: {e}"));
        assert!(
            out.status.success(),
            "fixture: `git {args:?}` in {} failed (exit {:?}): stdout={} stderr={}",
            dir.display(),
            out.status.code(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Every file named EXACTLY `<REPO_PRIMARY_LOCK_KEY>.lock` under `dir`
    /// (the transient `…​.lock.tmp.*` publish files are not matched).
    ///
    /// This observes what the lock code ITSELF published instead of re-deriving
    /// the path with `lock_path`/`repo_root`. Deriving the expectation from the
    /// function under test is precisely what makes a test unable to tell
    /// "worktree-scoped" from "main-worktree-scoped".
    fn published_repo_primary_locks(dir: &Path) -> Vec<PathBuf> {
        let mut found = Vec::new();
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    found.extend(published_repo_primary_locks(&p));
                } else if p
                    .file_name()
                    .map(|n| n == format!("{REPO_PRIMARY_LOCK_KEY}.lock").as_str())
                    .unwrap_or(false)
                {
                    found.push(p);
                }
            }
        }
        found.sort();
        found
    }

    // The repo-primary lock must be scoped to the REPOSITORY, not to whichever
    // checkout of it a process happens to be standing in. A real `git worktree
    // add` gives the main tree a `.git` DIRECTORY and the linked worktree a
    // `.git` FILE; both share ONE git index, ONE `.git/worktrees` admin dir and
    // ONE default branch, which is exactly what every holder of this lock
    // mutates (`worktree::merge`'s checkout+merge, the main-tree commit,
    // `git worktree prune`). So a mutator whose cwd is the main tree and a
    // mutator whose cwd is a linked worktree of the SAME repo must contend the
    // SAME lock file, and the second must be REFUSED while the first holds it.
    //
    // The assertion observes the SERIALIZATION behaviour (the contender is
    // refused), not merely path equality, because serialization is the property
    // the lock is bought for; the published-lock-file paths are reported
    // alongside it only to name the mechanism when it breaks. A failure is
    // observed FAST (an unshared lock is granted immediately); only the passing
    // path pays `RunLock::DEADLINE`, which is the honest cost of proving that a
    // live holder is waited out rather than reaped.
    #[test]
    fn repo_primary_lock_is_one_lock_across_a_repo_and_its_linked_worktree() {
        let base = std::env::temp_dir().join(format!(
            "condukt-repo-primary-worktree-{}-{}",
            std::process::id(),
            now_unix_nanos()
        ));
        let main_tree = base.join("main-tree");
        let linked = base.join("linked-wt");
        std::fs::create_dir_all(&main_tree).unwrap();

        git(&main_tree, &["init", "-q"]);
        git(&main_tree, &["config", "user.email", "t@t.t"]);
        git(&main_tree, &["config", "user.name", "t"]);
        git(&main_tree, &["config", "commit.gpgsign", "false"]);
        // `git worktree add` needs at least one commit to branch from.
        std::fs::write(main_tree.join("seed.txt"), "seed\n").unwrap();
        git(&main_tree, &["add", "seed.txt"]);
        git(&main_tree, &["commit", "-q", "-m", "seed"]);
        git(
            &main_tree,
            &[
                "worktree",
                "add",
                "-b",
                "linked-wt",
                linked
                    .to_str()
                    .expect("fixture: worktree path must be UTF-8"),
            ],
        );

        // Fixture invariants: without the real `.git`-file indirection this test
        // would be testing nothing at all.
        assert!(
            main_tree.join(".git").is_dir(),
            "fixture invariant: the main tree's .git must be a DIRECTORY, got {}",
            main_tree.join(".git").display()
        );
        assert!(
            linked.join(".git").is_file(),
            "fixture invariant: the linked worktree's .git must be a FILE (a \
             `gitdir:` pointer). That indirection — which makes `.exists()` true \
             at the worktree itself — is the whole subject of this test; got {}",
            linked.join(".git").display()
        );

        let cfg = test_cfg(base.join("state"));

        // Holder: a primary-repo critical section entered with cwd = MAIN TREE.
        let holder = match acquire_repo_primary(&cfg, &main_tree) {
            Ok(g) => g,
            Err(e) => panic!(
                "setup: the first repo-primary acquire (cwd = the main tree) must \
                 succeed on a fresh state dir; got: {e:#}"
            ),
        };
        assert!(
            holder.held(),
            "setup: the main-tree holder must genuinely hold the lock"
        );
        let while_main_holds = published_repo_primary_locks(&cfg.state_dir);

        // The property: a second primary-repo mutator standing in a LINKED
        // WORKTREE of the same repo must contend that same lock, so it is
        // refused rather than granted a second, independent lock.
        let contender = acquire_repo_primary(&cfg, &linked);
        // Snapshot WHILE the contender's guard (if it was wrongly granted one) is
        // still alive, so the failure message reports the lock files that actually
        // coexisted rather than what survived the contender's drop.
        let during_contention = published_repo_primary_locks(&cfg.state_dir);
        let granted = match contender {
            Ok(g) => {
                let held = g.held();
                drop(g);
                held
            }
            Err(_) => false,
        };

        assert!(
            !granted,
            "PROPERTY (the repo-primary lock is scoped to the REPOSITORY, not to a \
             checkout) violated: `acquire_repo_primary` with cwd = the LINKED \
             WORKTREE {} was GRANTED while a holder taken with cwd = the MAIN TREE \
             {} of the SAME repository was still alive. Those two cwds share one \
             git index, one `.git/worktrees` admin dir and one default branch — the \
             very things every holder of this lock mutates — so they must contend \
             ONE lock file and the second must be refused. Lock files published \
             under {}: while only the main tree held it {:?}; while the \
             linked-worktree acquire was outstanding {:?}. A SECOND, distinct path \
             there means the lock is keyed by `repo_root(cwd)`, which stops at the \
             first ancestor holding a `.git` ENTRY and therefore returns the LINKED \
             WORKTREE itself (there `.git` is a FILE), instead of the main worktree \
             root shared by both checkouts.",
            linked.display(),
            main_tree.display(),
            cfg.state_dir.display(),
            while_main_holds,
            during_contention
        );

        assert_eq!(
            during_contention.len(),
            1,
            "PROPERTY (one lock FILE per repository): a repo and its linked \
             worktree must address a single `{REPO_PRIMARY_LOCK_KEY}.lock` under a \
             single project-keyed directory; more than one means the two checkouts \
             were keyed as separate projects and never contended. Found {:?} under {}",
            during_contention,
            cfg.state_dir.display()
        );

        drop(holder);
        assert!(
            published_repo_primary_locks(&cfg.state_dir).is_empty(),
            "the repo-primary lock file must be released (removed) on guard drop"
        );

        std::fs::remove_dir_all(&base).ok();
    }
}
