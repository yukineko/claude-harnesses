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
use crate::store::project_key;
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
/// default_branch + merge) and `git worktree prune` — serializes on it instead
/// of racing on `main` (which today is only serialized by the upstream flow
/// backlog lock). Mirrors
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
/// Why not `repo_root`: `repo_root`
/// stops at the first ancestor holding a `.git` ENTRY, and in a linked worktree
/// `.git` is a FILE — so it returns the LINKED WORKTREE, giving each checkout
/// its own project key and therefore its own `__repo_primary__.lock`. Two
/// condukt processes would then each hold "the" primary lock and both mutate
/// the ONE thing this lock protects: the shared git index, the shared
/// `.git/worktrees` admin dir and the shared default branch. Under CLAUDE.md §8
/// every session works in its own linked worktree, so that split is the normal
/// case, not an edge one.
///
/// Run state has since been re-keyed the same way (backlog `43393ce2`, see
/// `state::run_dir`, which also keeps runs stored under the old per-checkout
/// key readable in place). Same opt-in shape the claim registry took in
/// `d83b0e8f`.
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
            let why = v.as_str();
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
/// fallible contract: cannot-acquire is `Err`, never an unheld guard — and a
/// configuration that cannot be loaded (config.toml unreadable/unparseable,
/// so the lock's configured state dir is unknown) is `Err` too, not a lock
/// taken in the default store where a peer would never see it.
pub fn acquire_repo_primary_loaded(cwd: &Path) -> Result<RunLock> {
    let cfg = Config::load().map_err(|e| {
        anyhow::anyhow!(
            "could not load condukt's configuration, so the repo-primary lock's \
             location is unknown; refusing to mutate the primary repo unlocked: {e}"
        )
    })?;
    acquire_repo_primary(&cfg, cwd)
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
///
/// `child_path` is an optional `PATH` for the spawned `kill` CHILD only.
/// `None` (production, via [`RunLock::acquire_at`]) inherits this process's environment;
/// `Some(p)` sets `PATH=p` on the child, which on Unix is also what the bare
/// `kill` is resolved against. Exists so a test can interpose a fake `kill`
/// without mutating the process-wide PATH (backlog 7c798579).
fn pid_alive_with_env_path(pid: u32, child_path: Option<&std::ffi::OsStr>) -> Determination<bool> {
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
    harness_core::boundary::run(&mut cmd).map(|out| out.code() == 0)
}

/// Lock file path keyed by the CHECKOUT (`repo_root(cwd)`) and a sanitised
/// key, so unrelated keys and unrelated checkouts never share a lock. Used only
/// by [`RunLock::acquire_or_skip`], which is test-only since its one production
/// caller (`repo_commit`'s per-index lock) was removed. It is NOT the run-state
/// lock any more: that sits beside the run's state file in `state::run_dir`
/// (see [`RunLock::acquire_run_state_or_skip`]).
#[cfg(test)]
fn lock_path(cfg: &Config, cwd: &Path, run_id: &str) -> PathBuf {
    lock_path_in_project(cfg, &crate::store::repo_root(cwd), run_id)
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

/// Minimum spacing between two liveness probes of the SAME holder pid while a
/// waiter polls a held lock (backlog e9262d91). The wait loop polls every 10ms,
/// and off Linux each probe is a `kill -0` fork+exec; probing on every poll
/// meant ~100 spawns per second per waiter, which under CPU saturation slowed
/// the waiter's own acquisition. Skipping a probe only ever means "keep
/// waiting" (the reap arm needs a positive `Known(false)`), so the throttle is
/// on the restrictive side: it can delay reaping a holder that died mid-wait by
/// about this interval plus one 10ms poll (and the probe's own run time), and
/// can never steal a live holder's lock.
const LIVENESS_PROBE_INTERVAL: Duration = Duration::from_millis(200);

/// Liveness of a lock holder `pid` as seen from this process. A holder pid
/// equal to our own pid is this very process — alive by observation, so no
/// `kill` is spawned (the in-process contention case, e.g. several threads of
/// one condukt invocation). Every other pid goes to
/// [`pid_alive_with_env_path`] with its three-valued answer unchanged.
fn holder_alive(pid: u32, child_path: Option<&std::ffi::OsStr>) -> Determination<bool> {
    if pid == std::process::id() {
        return Determination::known(true);
    }
    pid_alive_with_env_path(pid, child_path)
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
    /// Waits up to [`RunLock::DEADLINE`]. Keyed by `repo_root(cwd)` — the
    /// CHECKOUT. Its one production caller was `repo_commit::commit`, removed
    /// together with `condukt repo commit`, so it is compiled for tests only. Run state is NOT locked through here; see
    /// [`RunLock::acquire_run_state_or_skip`]. Primary-repo mutators in
    /// `worktree`/`main` lock via [`acquire_repo_primary`].
    #[cfg(test)]
    pub fn acquire_or_skip(cfg: &Config, cwd: &Path, run_id: &str) -> Option<Self> {
        Self::acquire_or_skip_at(lock_path(cfg, cwd, run_id), Self::DEADLINE)
    }

    /// The per-RUN state lock: `<run_dir>/<run>.lock`, beside the run's state
    /// file in [`crate::state::run_dir`] (the canonical main-worktree-root dir,
    /// or the legacy per-checkout dir a pre-`43393ce2` run still lives in). So
    /// every checkout that read-modify-writes ONE run file contends ONE lock —
    /// keying the lock by `repo_root(cwd)` like [`RunLock::acquire_or_skip`]
    /// would give a linked worktree and the main tree two different locks for
    /// the same shared file.
    ///
    /// Three answers: `Ok(Some)` held; `Ok(None)` contended/IO failure (the
    /// caller hard-skips); `Err` the run's dir itself cannot be determined (no
    /// fallback lock path is invented).
    pub fn acquire_run_state_or_skip(
        cfg: &Config,
        cwd: &Path,
        run_id: &str,
    ) -> Result<Option<Self>> {
        let dir = crate::state::run_dir(cfg, cwd, run_id)?;
        let path = dir.join(format!(
            "{}.lock",
            harness_core::store::safe_session(run_id)
        ));
        Ok(Self::acquire_or_skip_at(path, Self::DEADLINE))
    }

    /// [`RunLock::acquire_or_skip`] against an ALREADY-RESOLVED project root
    /// instead of a `cwd`.
    ///
    /// Its callers all guard something shared by every checkout of a repo,
    /// which must therefore be keyed by the MAIN worktree root:
    ///
    /// - the claim registry, for its reserved [`crate::claim::CLAIMS_LOCK_KEY`]
    ///   — every linked worktree read-modify-writes ONE `claims.json` and must
    ///   therefore contend ONE `__claims__.lock`;
    /// - [`acquire_repo_primary`], for [`REPO_PRIMARY_LOCK_KEY`] — every
    ///   checkout mutates ONE git index, ONE `.git/worktrees` admin dir and ONE
    ///   default branch, so they must contend ONE `__repo_primary__.lock`;
    /// - the escalation registry (`__escalations__`) and the precedent store
    ///   (`__precedents__`), whose single per-project file every checkout
    ///   read-modify-writes since backlog `43393ce2`.
    ///
    /// Run state is keyed by the main worktree root too (backlog `43393ce2`),
    /// but per RUN dir, through [`RunLock::acquire_run_state_or_skip`]. Same
    /// fallible contract: a failed acquisition is `None`, never a usable guard.
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
        Self::acquire_at_with_env_path(path, deadline, None)
    }

    /// [`RunLock::acquire_at`] with an optional `PATH` passed to the liveness
    /// probe's `kill` child only (see [`pid_alive_with_env_path`]). `None` is
    /// the production behaviour; the seam lets a test count `kill` spawns
    /// without mutating the process-wide PATH (backlog 7c798579).
    fn acquire_at_with_env_path(
        path: PathBuf,
        deadline: Duration,
        kill_path: Option<&std::ffi::OsStr>,
    ) -> Self {
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
        // The holder pid we last probed and when. A new holder pid is probed
        // immediately; the same holder is re-probed at most once per
        // `LIVENESS_PROBE_INTERVAL` (backlog e9262d91).
        let mut last_probe: Option<(u32, Instant)> = None;
        loop {
            match std::fs::hard_link(&tmp_path, &path) {
                Ok(()) => return RunLock { path: Some(path) },
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    // Someone holds the lock. Reap it only if we can positively
                    // confirm the owner pid is gone; otherwise wait for release.
                    //
                    // Reap ONLY on a positive "the owner is gone"
                    // (`Known(false)`). A live owner (`Known(true)`), an
                    // UNDETERMINED liveness (`kill` unspawnable / signalled —
                    // "I could not ask the OS"), an unreadable lock file, AND a
                    // probe skipped by the throttle all fall through to the
                    // wait arm below. Reaping on "cannot tell" would steal a
                    // live holder's lock — the exact fail-open this lock exists
                    // to close.
                    let holder_gone = match read_info(&path) {
                        Some(existing) => {
                            let due = match last_probe {
                                Some((pid, at)) => {
                                    pid != existing.pid || at.elapsed() >= LIVENESS_PROBE_INTERVAL
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
        // No wall-clock bound on acquisition (backlog `35d61d23`). This test's
        // property is mutual exclusion (no lost update; every RMW genuinely
        // holds the lock), NOT the timeout. A finite deadline here made the
        // test a scheduler race: pausing / starving the test process past it
        // made `acquire_at` correctly refuse and the `held()` assert below
        // fail with no lost update ever observed. The deadline's refusal
        // semantics are proven separately by the hard-skip tests above, and
        // the production `RunLock::DEADLINE` is untouched. Every holder is an
        // in-process thread whose guard releases on drop (including panic
        // unwind), so an unbounded wait only fails to return on a genuine
        // never-released lock — a hang, never a pass.
        const WAIT_UNTIL_ACQUIRED: Duration = Duration::MAX;

        std::thread::scope(|scope| {
            for _ in 0..THREADS {
                let lock_file = lock_file.clone();
                let counter = counter.clone();
                scope.spawn(move || {
                    for _ in 0..ITERS {
                        let g = RunLock::acquire_at(lock_file.clone(), WAIT_UNTIL_ACQUIRED);
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
    // mutator (merge / prune) contends, from whichever
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
    // mutates (`worktree::merge`'s checkout+merge and `git worktree prune`).
    // So a mutator whose cwd is the main tree and a
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

#[cfg(test)]
mod backlog_e9262d91 {
    //! backlog e9262d91: while waiting on a held lock, every 10ms poll calls
    //! `pid_alive_with_env_path`, which (off Linux) spawns `kill -0 <pid>` — ~100 fork+exec
    //! per second per waiter, up to ~1000 per acquisition, self-amplifying
    //! under load. Both fixes the ticket proposes (short-circuit a holder pid
    //! equal to our own; or check liveness only every few polls) bound the
    //! spawn rate.
    //!
    //! Observation: hold the lock under THIS process's pid (the in-process
    //! contention of `repo_primary_lock_serializes_concurrent_rmw_no_lost_update`),
    //! put a counting `kill` first on PATH, wait 1s, count spawns. The bound
    //! (at most 10 in 1s, i.e. at most one liveness probe per ~10 polls) is
    //! the contract chosen here; before the fix (652e3cec) it was ~one per
    //! poll.
    //!
    //! Contracts pinned below: a live holder is never stolen; a holder pid
    //! equal to our own spawns ZERO probes; a dead holder is reaped on its
    //! first probe (including one that dies mid-wait); and a holder pid that
    //! changes mid-wait is probed at once rather than after the throttle
    //! interval.
    use super::*;

    /// Every test in this module measures spawn COUNTS/RATES or reap timing,
    /// which a sibling waiter distorts when run concurrently (each waiter's
    /// fork+exec load slows the others' spawns). Serialize them all.
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn serial() -> std::sync::MutexGuard<'static, ()> {
        SERIAL.lock().unwrap_or_else(|p| p.into_inner())
    }

    #[cfg(unix)]
    #[test]
    fn lock_wait_does_not_spawn_kill_on_every_poll() {
        let _serial = serial();
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::TempDir::new().unwrap();
        let bin = tmp.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let count = tmp.path().join("kill.count");
        let fake = bin.join("kill");
        std::fs::write(
            &fake,
            format!(
                "#!/bin/sh\necho x >> '{}'\nexec /bin/kill \"$@\"\n",
                count.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

        let lock = tmp.path().join("held.lock");
        let info = LockInfo {
            pid: std::process::id(),
            run_id: "held".into(),
            acquired_at: now_unix(),
        };
        std::fs::write(&lock, serde_json::to_string(&info).unwrap()).unwrap();

        // The counting `kill` goes first on the CHILD's PATH only; the
        // process-wide PATH is never mutated (backlog 7c798579).
        let mut parts = vec![bin.clone()];
        if let Some(p) = std::env::var_os("PATH") {
            parts.extend(std::env::split_paths(&p));
        }
        let kill_path = std::env::join_paths(parts).unwrap();
        let got = RunLock::acquire_at_with_env_path(
            lock.clone(),
            Duration::from_secs(1),
            Some(&kill_path),
        );

        assert!(
            !got.held(),
            "fixture precondition: a live holder must not be stolen"
        );
        assert!(
            lock.exists(),
            "fixture precondition: the held lock must survive the wait"
        );
        let spawns = std::fs::read_to_string(&count)
            .map(|s| s.lines().count())
            .unwrap_or(0);
        assert!(
            spawns <= 10,
            "waiting 1s on a held lock spawned `kill` {spawns} times (one fork+exec per 10ms poll)"
        );
    }

    /// Counting fake `kill` placed first on a CHILD-only PATH. Each spawn
    /// appends one line to the count file: `alive` (probe exit 0) or `dead`
    /// (non-zero). Returns
    /// `(tempdir, count_file, child_path)`; the tempdir must outlive the wait.
    #[cfg(unix)]
    fn counting_kill_path() -> (tempfile::TempDir, PathBuf, std::ffi::OsString) {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::TempDir::new().unwrap();
        let bin = tmp.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let count = tmp.path().join("kill.count");
        let fake = bin.join("kill");
        std::fs::write(
            &fake,
            format!(
                "#!/bin/sh\n/bin/kill \"$@\"\nrc=$?\n\
                 if [ $rc -eq 0 ]; then echo alive >> '{c}'; else echo dead >> '{c}'; fi\n\
                 exit $rc\n",
                c = count.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut parts = vec![bin];
        if let Some(p) = std::env::var_os("PATH") {
            parts.extend(std::env::split_paths(&p));
        }
        let kill_path = std::env::join_paths(parts).unwrap();
        // Warm-up: the FIRST exec of a freshly written script is slow on
        // macOS (observed ~300ms idle, 0.7-2.4s under load), which made a
        // wall-clock reap bound measure the OS, and made a 0.3s holder die
        // before the first probe ran. Exec it once, then reset the log.
        let warm = std::process::Command::new(&fake)
            .args(["-0", &std::process::id().to_string()])
            .output()
            .expect("fixture: warm-up exec of the fake `kill`");
        assert!(
            warm.status.success(),
            "fixture: warm-up `kill -0 <self>` must exit 0"
        );
        std::fs::remove_file(&count).expect("fixture: reset the probe log after warm-up");
        (tmp, count, kill_path)
    }

    #[cfg(unix)]
    fn write_lock(path: &Path, pid: u32) {
        let info = LockInfo {
            pid,
            run_id: "held".into(),
            acquired_at: now_unix(),
        };
        std::fs::write(path, serde_json::to_string(&info).unwrap()).unwrap();
    }

    /// Kills and reaps the foreign holder on every exit path (incl. panic).
    #[cfg(unix)]
    struct ChildGuard(std::process::Child);
    #[cfg(unix)]
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    /// The holder is a LIVE FOREIGN process (not this test's pid), so a fix
    /// that only short-circuits `existing.pid == std::process::id()` does not
    /// satisfy it: the liveness probe itself must be rate-limited, and the
    /// live holder must still never be stolen.
    #[cfg(unix)]
    #[test]
    fn lock_wait_on_live_foreign_holder_is_throttled_and_never_steals() {
        let _serial = serial();
        let (tmp, count, kill_path) = counting_kill_path();
        let child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("fixture: spawn `sleep 30` as the foreign lock holder");
        let guard = ChildGuard(child);
        let holder_pid = guard.0.id();

        let lock = tmp.path().join("held.lock");
        write_lock(&lock, holder_pid);

        let got = RunLock::acquire_at_with_env_path(
            lock.clone(),
            Duration::from_secs(1),
            Some(&kill_path),
        );

        assert!(
            !got.held(),
            "a LIVE foreign holder (pid {holder_pid}) was stolen: the waiter acquired the lock"
        );
        let on_disk: LockInfo = serde_json::from_str(
            &std::fs::read_to_string(&lock)
                .expect("the live foreign holder's lock file must survive the wait"),
        )
        .expect("the held lock file must still parse");
        assert_eq!(
            on_disk.pid, holder_pid,
            "the lock file no longer names the live foreign holder (stolen/rewritten)"
        );
        let spawns = std::fs::read_to_string(&count)
            .map(|s| s.lines().count())
            .unwrap_or(0);
        assert!(
            spawns <= 10,
            "waiting 1s on a lock held by a live FOREIGN pid spawned `kill` {spawns} times \
             (bound: <= 10 per second of waiting)"
        );
        drop(guard);
    }

    /// Reads the probe log written by [`counting_kill_path`]'s fake `kill`:
    /// `(alive_probes, dead_probes)`.
    #[cfg(unix)]
    fn probe_log(count: &Path) -> (usize, usize) {
        let log = std::fs::read_to_string(count).unwrap_or_default();
        let alive = log.lines().filter(|l| *l == "alive").count();
        let dead = log.lines().filter(|l| *l == "dead").count();
        (alive, dead)
    }

    /// Guard against a throttling fix that breaks reaping: a lock held by an
    /// already-DEAD pid must be reaped on the FIRST liveness probe and
    /// acquired. Count-based, not wall-clock: one fork+exec of the shell
    /// wrapper alone was observed to take 0.7–2.4s under load (avg 10–16), so
    /// a latency bound measured the machine, not the reap.
    #[cfg(unix)]
    #[test]
    fn lock_wait_on_dead_holder_still_reaps_promptly() {
        let _serial = serial();
        let (tmp, count, kill_path) = counting_kill_path();
        let mut child = std::process::Command::new("true")
            .spawn()
            .expect("fixture: spawn a short-lived child");
        let dead_pid = child.id();
        let status = child.wait().expect("fixture: wait for the child to exit");
        assert!(status.success(), "fixture: `true` must exit 0");

        let lock = tmp.path().join("held.lock");
        write_lock(&lock, dead_pid);

        let got = RunLock::acquire_at_with_env_path(
            lock.clone(),
            Duration::from_secs(5),
            Some(&kill_path),
        );

        assert!(
            got.held(),
            "a lock held by DEAD pid {dead_pid} was not reaped within the 5s deadline"
        );
        let (alive, dead) = probe_log(&count);
        assert_eq!(
            (alive, dead),
            (0, 1),
            "a DEAD holder (pid {dead_pid}) must be reaped on the first liveness probe: \
             expected exactly 1 `kill` spawn reporting dead, got {alive} alive + {dead} dead"
        );
    }

    /// Guard: a holder that dies MID-wait is reaped on the first probe that
    /// observes its death (no extra probes after death) and the lock is
    /// acquired before the deadline. A reaper thread `wait()`s the child so
    /// it does not linger as a zombie (`kill -0` succeeds on a zombie).
    #[cfg(unix)]
    #[test]
    fn lock_wait_on_holder_dying_mid_wait_reaps_on_first_dead_probe() {
        let _serial = serial();
        let (tmp, count, kill_path) = counting_kill_path();
        let mut child = std::process::Command::new("sleep")
            .arg("1")
            .spawn()
            .expect("fixture: spawn `sleep 1` as the soon-to-die holder");
        let holder_pid = child.id();
        let reaper = std::thread::spawn(move || child.wait());

        let lock = tmp.path().join("held.lock");
        write_lock(&lock, holder_pid);

        let deadline = Duration::from_secs(5);
        let start = Instant::now();
        let got = RunLock::acquire_at_with_env_path(lock.clone(), deadline, Some(&kill_path));
        let elapsed = start.elapsed();
        let status = reaper
            .join()
            .expect("fixture: reaper thread panicked")
            .expect("fixture: wait for the holder to exit");
        assert!(status.success(), "fixture: `sleep 1` must exit 0");

        let (alive, dead) = probe_log(&count);
        assert!(
            alive >= 1,
            "fixture inconclusive: the holder (pid {holder_pid}) was never probed while alive \
             ({alive} alive + {dead} dead), so this did not exercise a MID-wait death"
        );
        assert!(
            got.held(),
            "holder pid {holder_pid} died ~1s into the wait but the lock was not acquired \
             before the {deadline:?} deadline (elapsed {elapsed:?}; {alive} alive + {dead} dead probes)"
        );
        assert_eq!(
            dead, 1,
            "the holder (pid {holder_pid}) must be reaped on the FIRST probe that observes its \
             death: got {dead} dead probes ({alive} alive)"
        );
    }

    /// A holder pid equal to OUR OWN pid is this very process: alive by
    /// observation, so the waiter must spawn ZERO `kill` probes (not merely a
    /// throttled few), and must still neither acquire nor disturb the lock.
    #[cfg(unix)]
    #[test]
    fn lock_wait_on_own_pid_holder_spawns_no_probe() {
        let _serial = serial();
        let (tmp, count, kill_path) = counting_kill_path();
        let own = std::process::id();
        let lock = tmp.path().join("held.lock");
        write_lock(&lock, own);

        let got = RunLock::acquire_at_with_env_path(
            lock.clone(),
            Duration::from_millis(500),
            Some(&kill_path),
        );

        assert!(
            !got.held(),
            "a lock held under our OWN pid {own} (a live in-process holder) was acquired/stolen"
        );
        let on_disk: LockInfo = serde_json::from_str(
            &std::fs::read_to_string(&lock).expect("the own-pid holder's lock must survive"),
        )
        .expect("the held lock file must still parse");
        assert_eq!(
            on_disk.pid, own,
            "the lock file no longer names the own-pid holder (stolen/rewritten)"
        );
        let (alive, dead) = probe_log(&count);
        assert_eq!(
            alive + dead,
            0,
            "waiting 500ms on a lock held by our OWN pid spawned `kill` {} times \
             ({alive} alive + {dead} dead); an own-pid holder must be judged alive \
             with zero spawns",
            alive + dead
        );
    }

    /// A holder pid that CHANGES mid-wait is probed at once, not after the
    /// same-holder throttle interval (`LIVENESS_PROBE_INTERVAL`).
    ///
    /// Choreography, driven by the fake `kill` itself (no sleeps, no helper
    /// thread): the lock starts held by a live foreign pid A. The FIRST probe
    /// of A answers "alive" and, before returning, atomically renames a lock
    /// naming a dead pid B into place. A correct waiter then probes B on the
    /// next poll (~10ms), reaps it, and acquires. A waiter that treats B as
    /// the same holder cannot probe again until `LIVENESS_PROBE_INTERVAL`
    /// after A's probe started, which is past the deadline below, so it must
    /// give up WITHOUT ever probing B.
    ///
    /// The deadline must sit below `LIVENESS_PROBE_INTERVAL`, so this test is
    /// timing-bound in one direction: if A's probe alone takes longer than
    /// the deadline (heavy load), the waiter gives up before it can probe B
    /// even when correct. That case is detected (A's marker mtime vs. the
    /// wait start) and reported as INCONCLUSIVE — still a failure, never a
    /// pass.
    #[cfg(unix)]
    #[test]
    fn lock_wait_probes_a_changed_holder_pid_immediately() {
        use std::os::unix::fs::PermissionsExt;
        let _serial = serial();
        const DEADLINE: Duration = Duration::from_millis(120);
        assert!(
            DEADLINE < LIVENESS_PROBE_INTERVAL,
            "fixture: the deadline must be below the same-holder throttle interval"
        );

        let tmp = tempfile::TempDir::new().unwrap();
        let lock = tmp.path().join("held.lock");
        let log = tmp.path().join("kill.log");
        let flag = tmp.path().join("rewritten.flag");
        let b_json = tmp.path().join("b.json");
        let b_stage = tmp.path().join("b.stage");

        let holder_a = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("fixture: spawn `sleep 30` as live holder A");
        let guard = ChildGuard(holder_a);
        let pid_a = guard.0.id();
        let mut short = std::process::Command::new("true")
            .spawn()
            .expect("fixture: spawn a short-lived child for dead pid B");
        let pid_b = short.id();
        assert!(
            short.wait().expect("fixture: reap B").success(),
            "fixture: `true` must exit 0"
        );

        write_lock(&lock, pid_a);
        write_lock(&b_json, pid_b);

        let bin = tmp.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let fake = bin.join("kill");
        std::fs::write(
            &fake,
            format!(
                "#!/bin/sh\n/bin/kill \"$@\"\nrc=$?\n\
                 if [ $rc -eq 0 ]; then echo \"alive $2\" >> '{log}'; \
                 else echo \"dead $2\" >> '{log}'; fi\n\
                 if [ \"$2\" = \"{pid_a}\" ] && [ ! -e '{flag}' ]; then \
                 : > '{flag}'; cp '{b_json}' '{b_stage}' && mv '{b_stage}' '{lock}'; fi\n\
                 exit $rc\n",
                log = log.display(),
                flag = flag.display(),
                b_json = b_json.display(),
                b_stage = b_stage.display(),
                lock = lock.display(),
            ),
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        // Warm-up (first exec of a fresh script is slow on macOS); probes our
        // own pid, so it neither matches A nor touches the lock. Reset the log.
        let warm = std::process::Command::new(&fake)
            .args(["-0", &std::process::id().to_string()])
            .output()
            .expect("fixture: warm-up exec of the fake `kill`");
        assert!(warm.status.success(), "fixture: warm-up must exit 0");
        std::fs::remove_file(&log).expect("fixture: reset the probe log after warm-up");
        let mut parts = vec![bin];
        if let Some(p) = std::env::var_os("PATH") {
            parts.extend(std::env::split_paths(&p));
        }
        let kill_path = std::env::join_paths(parts).unwrap();

        let wall_start = std::time::SystemTime::now();
        let got = RunLock::acquire_at_with_env_path(lock.clone(), DEADLINE, Some(&kill_path));

        let probes: Vec<String> = std::fs::read_to_string(&log)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect();
        let a_probe_done = std::fs::metadata(&flag)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(wall_start).ok());
        if !got.held() {
            if let Some(took) = a_probe_done {
                assert!(
                    took < DEADLINE,
                    "INCONCLUSIVE (not a pass): A's first probe alone took {took:?} >= the \
                     {DEADLINE:?} deadline, so the waiter could not reach B's probe in time \
                     even if correct (probes: {probes:?})"
                );
            }
        }
        assert!(
            got.held(),
            "the holder changed from live pid {pid_a} to dead pid {pid_b} mid-wait, but the \
             lock was not acquired within {DEADLINE:?}: the changed holder was not probed \
             immediately (probes: {probes:?})"
        );
        assert_eq!(
            probes,
            vec![format!("alive {pid_a}"), format!("dead {pid_b}")],
            "expected exactly one probe of A (alive) then one of B (dead, reaped on first probe)"
        );
        drop(guard);
    }
}
