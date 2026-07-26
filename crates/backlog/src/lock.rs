use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use harness_core::config::base_dir;
use harness_core::progress::{self, Liveness};
use harness_core::verdict::Determination;

use crate::store::canonicalize_project;

/// Derive a filesystem-safe, per-project lock-file identity: canonicalize the
/// project the same way `store::add_with_weight`/`list` do (so `--project
/// "$PWD"` from any subdirectory or worktree of the SAME repo lands on the
/// same slug), then hash it with the shared FNV-1a so the filename never
/// contains path separators. `backlog` is explicitly a cross-project queue
/// (its own `--help` says so), so a single global `run.lock` blocked a
/// session working on project A whenever ANY other session held the lock for
/// unrelated project B — real observed friction, not a hypothetical (a `/flow`
/// run on `harness` stood down because a concurrent session held the lock for
/// `ai-aegis`, a completely different repo). Scoping the lock file per project
/// lets independent projects' `/flow` loops run concurrently while still
/// serializing two sessions racing on the SAME project's queue.
pub(crate) fn project_slug(project: &str) -> String {
    let canonical = canonicalize_project(project);
    format!("{:016x}", harness_core::hash::fnv1a64(canonical.as_bytes()))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockInfo {
    pub session_id: String,
    /// OS pid of the process that last (re)acquired this lock. Kept only for
    /// observability (`backlog lock status`) — NOT used to judge staleness.
    /// The `backlog` binary is a one-shot CLI invocation that exits immediately
    /// after this command returns, so by the time any later command reads this
    /// lock the recorded pid is already dead, whether or not the session that
    /// holds it is still working. Staleness is judged by `heartbeat_at`
    /// instead (see `is_stale`), mirroring condukt's task-claim registry and
    /// overwatch's lease registry.
    pub pid: u32,
    pub project: String,
    pub acquired_at: i64,
    /// Unix timestamp of the last heartbeat. Refreshed by `backlog lock
    /// heartbeat` while the holding session is still active. Absent on locks
    /// written before this field existed (`#[serde(default)]` -> 0), which
    /// reads as maximally stale — safe, since such a lock predates any
    /// session that could still legitimately hold it.
    #[serde(default)]
    pub heartbeat_at: i64,
}

#[derive(Debug)]
pub enum LockStatus {
    /// Lock is held by a session whose heartbeat is still fresh.
    Active(LockInfo),
    /// Lock file exists but its heartbeat is older than the stale TTL.
    Stale(LockInfo),
    /// No lock file. This is an *observation* — the file is genuinely absent,
    /// so nobody holds the lock.
    None,
    /// The lock could not be judged: a lock file exists but is unreadable or
    /// unparseable, or the locks directory could not be scanned. This is NOT
    /// `None`. Collapsing it into `None` would report "free" to callers that
    /// use this to decide whether to start a second driver, which is the
    /// permissive answer to a question we did not manage to answer.
    Undetermined(String),
}

/// A lock is stale once its heartbeat is older than this many seconds without
/// a refresh. Mirrors condukt's `stuck_ttl_secs` / overwatch's
/// `LEASE_TTL_SECS` (both default to 1800s / 30min) for consistency across
/// the harness's cross-session staleness registries.
pub(crate) const LOCK_STALE_TTL_SECS: i64 = 1800;

fn is_stale(info: &LockInfo, now: i64) -> bool {
    now.saturating_sub(info.heartbeat_at) > LOCK_STALE_TTL_SECS
}

/// Directory holding this reaper's per-holder progress snapshots. Beside the
/// locks (under the same base or the test override) so a test that overrides
/// `lock_dir` gets an isolated progress store too.
fn progress_store_dir(lock_dir: Option<&Path>) -> PathBuf {
    match lock_dir {
        Some(d) => d.join("progress"),
        None => base_dir("backlog").join("progress"),
    }
}

/// Judge whether the current holder of a **heartbeat-stale** lock is actually
/// making progress, or has genuinely stalled. This is the correctness core of
/// the reap fix: a stale heartbeat alone no longer authorizes a reap, because a
/// live-but-quiet session (fresh git commits / a growing transcript, heartbeat
/// merely lapsed) would be force-stolen. Signals: the project's git HEAD and the
/// holder session's transcript growth. If either is unreadable/absent the
/// fingerprint is `Undetermined`; a single sample is `Undetermined`; only a
/// fingerprint frozen across the multi-sample window is `Known(Stalled)`.
///
/// A reaper reaps **iff** this returns `Known(Stalled)`; `Progressing` and
/// `Undetermined` both protect the holder (never reap).
fn holder_progress(
    existing: &LockInfo,
    now: i64,
    lock_dir: Option<&Path>,
) -> Determination<Liveness> {
    #[cfg(test)]
    if let Some(forced) = test_hook::forced_progress() {
        return forced;
    }
    let head = progress::git_head_signal(Path::new(&existing.project));
    let transcript = progress::session_transcript_signal(&existing.session_id);
    let current =
        progress::fingerprint_from_signals(vec![("git-head", head), ("transcript", transcript)]);
    let key = format!(
        "lock:{}:{}",
        project_slug(&existing.project),
        existing.session_id
    );
    progress::sample(
        &progress_store_dir(lock_dir),
        &key,
        current,
        now,
        progress::window_secs(progress::DEFAULT_WINDOW_SECS),
    )
}

/// Compute the current holder's PROGRESS verdict for `project` as a
/// [`Determination<Liveness>`], using the SAME single-call delta-vs-snapshot
/// machinery as [`holder_progress`] / [`probe_at`] (git-head + transcript
/// signals sampled against the persisted prior snapshot under
/// `progress_store_dir`). This is the verdict `backlog lock status` embeds in
/// an active-liveness object so heartbeat-only liveness cannot be rendered
/// ([`crate::liveness::status_value`]).
///
/// When there is no holder — or the lock file is present but unreadable — there
/// is no holder to judge, so the result is `Undetermined` (never a fabricated
/// `Progressing`): status renders that branch without a liveness claim. A held
/// holder whose signals are all unreadable, or a single sample, is likewise
/// `Undetermined` (CLAUDE.md §3).
///
/// This is READ-ONLY observability: it advances the same sample state machine
/// as `probe`, but it does NOT reap and does NOT change the reap-gate
/// invariant — a plain acquire still reaps ONLY when [`holder_progress`]
/// returns `Known(Stalled)`.
pub fn holder_progress_verdict_at(
    project: &str,
    lock_dir: Option<&Path>,
) -> Determination<Liveness> {
    let path = match lock_dir {
        Some(d) => lock_path_for(d, project),
        None => lock_path(project),
    };
    let existing = match read_lock(&path) {
        Some(info) => info,
        None => {
            let why = if path.exists() {
                "lock file present but unreadable"
            } else {
                "no lock held for this project"
            };
            return Determination::undetermined(why);
        }
    };
    holder_progress(&existing, now_unix(), lock_dir)
}

/// Compute the current holder's progress verdict using the default lock path.
/// See [`holder_progress_verdict_at`].
pub fn holder_progress_verdict(project: &str) -> Determination<Liveness> {
    holder_progress_verdict_at(project, None)
}

/// `#[cfg(test)]` seam that lets a unit test force the progress verdict of a
/// holder, so the reap-gate truth table (Stalled ⇒ reap; Progressing /
/// Undetermined ⇒ protect) is exercised deterministically without standing up a
/// real git repo + live transcript per case. Thread-local, so parallel cargo
/// test threads never race, and compiled out of production entirely (the check
/// in [`holder_progress`] is `#[cfg(test)]`).
#[cfg(test)]
mod test_hook {
    use super::*;
    use std::cell::RefCell;

    thread_local! {
        static FORCED: RefCell<Option<Determination<Liveness>>> = const { RefCell::new(None) };
    }

    pub(super) fn forced_progress() -> Option<Determination<Liveness>> {
        FORCED.with(|c| c.borrow().clone())
    }

    /// Run `body` with the holder progress verdict forced to `v`, restoring the
    /// prior state afterward (RAII so a panic still clears it).
    pub(super) fn with_forced<R>(v: Determination<Liveness>, body: impl FnOnce() -> R) -> R {
        struct Guard;
        impl Drop for Guard {
            fn drop(&mut self) {
                FORCED.with(|c| *c.borrow_mut() = None);
            }
        }
        FORCED.with(|c| *c.borrow_mut() = Some(v));
        let _g = Guard;
        body()
    }
}

fn locks_dir() -> PathBuf {
    base_dir("backlog").join("locks")
}

fn locks_dir_for(base: &Path) -> PathBuf {
    base.join("locks")
}

fn lock_path(project: &str) -> PathBuf {
    locks_dir().join(format!("{}.lock", project_slug(project)))
}

fn lock_path_for(base: &Path, project: &str) -> PathBuf {
    locks_dir_for(base).join(format!("{}.lock", project_slug(project)))
}

/// List every currently-present lock file's path, for the project-agnostic
/// "is ANY project's driver active" scan (`status_any`).
///
/// A `locks/` directory that has never been created reads as "no locks" — that
/// is an observation, since the directory is created lazily on first `acquire`
/// and its absence means nobody has ever locked anything. Any OTHER read
/// failure is `Err`: we did not manage to look, and an empty list would be read
/// downstream as "no driver is active", which is the permissive answer to an
/// unanswered question.
fn all_lock_files(dir: &Path) -> std::result::Result<Vec<PathBuf>, String> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("reading {}: {e}", dir.display())),
    };
    let mut out = Vec::new();
    for entry in entries {
        let path = entry
            .map_err(|e| format!("reading {}: {e}", dir.display()))?
            .path();
        if path.extension().is_some_and(|ext| ext == "lock") {
            out.push(path);
        }
    }
    Ok(out)
}

fn read_lock(path: &Path) -> Option<LockInfo> {
    let txt = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&txt).ok()
}

fn now_unix() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Acquire the lock. Returns an error if the lock is currently active.
/// `lock_dir` allows tests to override the directory; pass `None` to use the
/// default `~/.backlog/` location.
///
/// Acquisition is atomic: the lock file is created with `create_new` (O_EXCL),
/// so two processes racing to create the same lock cannot both succeed — exactly
/// one wins the create, the loser sees `AlreadyExists`. There is no longer a
/// check-then-write window (the previous TOCTOU bug): the create itself is the
/// check.
///
/// **Reaping is progress-gated, not heartbeat-gated.** A lock whose heartbeat is
/// past the TTL is reaped ONLY when the holder's *progress* is confirmed stalled
/// (see [`holder_progress`]): git HEAD and the holder's transcript frozen across
/// the multi-sample window. A holder that is still progressing — or whose
/// progress cannot yet be determined (a single sample, an unreadable signal) —
/// is NOT reaped even with a stale heartbeat, because a stale heartbeat can
/// accompany a live-but-quiet session. `--force` ([`acquire_forced_at`]) is the
/// human override that reaps regardless. The retry loop is bounded, so a genuine
/// dead holder (frozen signals) ages out across successive acquire attempts and
/// never blocks acquisition forever.
pub fn acquire_at(
    session_id: &str,
    pid: u32,
    project: &str,
    lock_dir: Option<&Path>,
) -> Result<()> {
    acquire_inner(session_id, pid, project, lock_dir, false)
}

/// Force-acquire the lock, displacing even a *live* holder. This is the
/// documented `--force` ("強制奪取") escape hatch: a human has decided the
/// current holder — e.g. an abandoned session whose process is still alive —
/// should be taken over. Unlike [`acquire_at`], which reaps a stale lock only
/// once the holder's progress is confirmed stalled, this reaps the existing lock
/// regardless of liveness or progress.
/// The publish step is still atomic, and the steal happens *inside* the bounded
/// retry loop, so a competitor that re-grabs the lock in the race window is
/// itself displaced (up to the attempt cap).
pub fn acquire_forced_at(
    session_id: &str,
    pid: u32,
    project: &str,
    lock_dir: Option<&Path>,
) -> Result<()> {
    acquire_inner(session_id, pid, project, lock_dir, true)
}

/// Force-acquire using the default lock path. See [`acquire_forced_at`].
pub fn acquire_forced(session_id: &str, pid: u32, project: &str) -> Result<()> {
    acquire_forced_at(session_id, pid, project, None)
}

/// Shared acquire implementation. `force = true` steals any holder's lock (the
/// `--force` path); `force = false` reaps a stale-heartbeat lock ONLY when the
/// holder's progress is confirmed stalled ([`holder_progress`]).
fn acquire_inner(
    session_id: &str,
    pid: u32,
    project: &str,
    lock_dir: Option<&Path>,
    force: bool,
) -> Result<()> {
    let path = match lock_dir {
        Some(d) => lock_path_for(d, project),
        None => lock_path(project),
    };

    // Ensure directory exists.
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create lock dir {}", parent.display()))?;
    }

    let now = now_unix();
    let info = LockInfo {
        session_id: session_id.to_string(),
        pid,
        project: project.to_string(),
        acquired_at: now,
        heartbeat_at: now,
    };
    // Serialize the lock contents into a temp file *before* publishing it, then
    // expose it atomically. `create_new` on the temp file gives us a private,
    // exclusively-owned path (the pid+nanos suffix makes collisions effectively
    // impossible), and a hard link from temp -> final path is the atomic
    // publish: link(2) fails with EEXIST if the final path already exists, so
    // exactly one racer can publish. Critically the file is *fully written*
    // before it is ever visible at the final path, so a concurrent reader can
    // never observe an empty/partial lock and misjudge it as stale.
    let json = serde_json::to_string_pretty(&info)?;
    let tmp_path = path.with_extension(format!("tmp.{}.{}", pid, now_unix_nanos()));
    {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)
            .with_context(|| format!("create temp lock file {}", tmp_path.display()))?;
        f.write_all(json.as_bytes())
            .with_context(|| format!("write temp lock file {}", tmp_path.display()))?;
        f.sync_all().ok();
    }
    // Ensure the temp file is cleaned up on every exit path.
    let _guard = TmpGuard(&tmp_path);

    // Bound the stale-reap/retry loop so a pathological race (another process
    // re-creating the lock right after we reap it) cannot spin forever.
    const MAX_ATTEMPTS: u32 = 8;
    for _ in 0..MAX_ATTEMPTS {
        // Atomic publish: link only succeeds if `path` does not yet exist.
        match std::fs::hard_link(&tmp_path, &path) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                // Someone else published a lock. Inspect it. We only reap when
                // its heartbeat is older than the stale TTL; an unreadable/
                // partial read is treated as "still being written" (active)
                // so we never delete a live holder's lock.
                match read_lock(&path) {
                    Some(existing) if !is_stale(&existing, now_unix()) => {
                        if force {
                            // --force: displace even a live holder, then retry
                            // the atomic publish.
                            let _ = std::fs::remove_file(&path);
                            continue;
                        }
                        anyhow::bail!(
                            "lock already held by session {} (pid {}, project {})",
                            existing.session_id,
                            existing.pid,
                            existing.project
                        );
                    }
                    Some(existing) => {
                        // Heartbeat is stale — but a stale heartbeat NO LONGER
                        // authorizes a reap on its own. A live-but-quiet holder
                        // (still committing / still growing its transcript, only
                        // its heartbeat lapsed) would otherwise be force-stolen
                        // (the memory scar this closes). Reap ONLY on confirmed
                        // non-progress; Progressing / Undetermined protect the
                        // holder. `--force` is the human override and still reaps.
                        if force {
                            let _ = std::fs::remove_file(&path);
                            continue;
                        }
                        match holder_progress(&existing, now_unix(), lock_dir) {
                            Determination::Known(Liveness::Stalled) => {
                                // Confirmed stalled across the multi-sample window: reap and retry.
                                let _ = std::fs::remove_file(&path);
                                continue;
                            }
                            verdict => {
                                anyhow::bail!(
                                    "lock held by session {} (project {}): heartbeat is stale but \
                                     progress is not confirmed stalled ({}); refusing to reap a \
                                     possibly-live holder — use --force to override",
                                    existing.session_id,
                                    existing.project,
                                    match verdict {
                                        Determination::Known(Liveness::Progressing) =>
                                            "still progressing".to_string(),
                                        Determination::Undetermined(why) =>
                                            format!("undetermined: {why}"),
                                        Determination::Known(Liveness::Stalled) => unreachable!(),
                                    }
                                );
                            }
                        }
                    }
                    None => {
                        // Unreadable: a writer is mid-publish (impossible with
                        // link, but possible if some external actor wrote a
                        // partial file). Briefly wait for it to settle, then
                        // re-judge on the next iteration without deleting it.
                        std::thread::sleep(std::time::Duration::from_millis(5));
                        continue;
                    }
                }
            }
            Err(e) => {
                return Err(e).with_context(|| format!("publish lock file {}", path.display()));
            }
        }
    }

    anyhow::bail!(
        "could not acquire lock at {} after {} attempts (contended/stale-thrashing)",
        path.display(),
        MAX_ATTEMPTS
    )
}

fn now_unix_nanos() -> u128 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// Removes a temp lock file when dropped, on every exit path.
struct TmpGuard<'a>(&'a Path);
impl Drop for TmpGuard<'_> {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(self.0);
    }
}

/// Acquire using the default lock path.
pub fn acquire(session_id: &str, pid: u32, project: &str) -> Result<()> {
    acquire_at(session_id, pid, project, None)
}

/// Release the lock.  No-op if no lock file exists.
/// `lock_dir` allows tests to override the directory.
pub fn release_at(project: &str, lock_dir: Option<&Path>) -> Result<()> {
    let path = match lock_dir {
        Some(d) => lock_path_for(d, project),
        None => lock_path(project),
    };
    if path.exists() {
        std::fs::remove_file(&path)
            .with_context(|| format!("remove lock file {}", path.display()))?;
    }
    Ok(())
}

/// Release using the default lock path.
pub fn release(project: &str) -> Result<()> {
    release_at(project, None)
}

/// Return the current lock status for a specific project.
/// `lock_dir` allows tests to override the directory.
pub fn status_at(project: &str, lock_dir: Option<&Path>) -> LockStatus {
    let path = match lock_dir {
        Some(d) => lock_path_for(d, project),
        None => lock_path(project),
    };
    status_from_path(&path)
}

fn status_from_path(path: &Path) -> LockStatus {
    match read_lock(path) {
        Some(info) => {
            if is_stale(&info, now_unix()) {
                LockStatus::Stale(info)
            } else {
                LockStatus::Active(info)
            }
        }
        // Distinguish "no lock file" (an observation: nobody holds it) from
        // "there IS a lock file but I could not read/parse it" (not an
        // observation about the holder at all).
        None if !path.exists() => LockStatus::None,
        None => LockStatus::Undetermined(format!("unreadable lock file {}", path.display())),
    }
}

/// Return the lock status for a specific project, using the default lock path.
pub fn status(project: &str) -> LockStatus {
    status_at(project, None)
}

/// Return whether ANY project's lock is currently active or stale-but-present,
/// scanning every lock file under `locks/` rather than a single project's slug.
/// This preserves `daily`'s "is any driver active anywhere on the machine"
/// bare `backlog lock status` behavior (no `--project` flag) after the lock
/// file layout became per-project: `daily` doesn't know or care which project
/// a driver is working on, only whether one is running. Picks the single
/// most "alive" result across all lock files: `Active` anywhere wins over
/// `Undetermined` (an Active lock already answers "yes, a driver is active"),
/// which wins over `Stale`, which wins over `None`. `Undetermined` outranks
/// `Stale`/`None` because both of those are read downstream as "not active" —
/// a permissive answer we are not entitled to give when we failed to look.
pub fn status_any_at(lock_dir: Option<&Path>) -> LockStatus {
    let dir = match lock_dir {
        Some(d) => locks_dir_for(d),
        None => locks_dir(),
    };
    let files = match all_lock_files(&dir) {
        Ok(f) => f,
        Err(why) => return LockStatus::Undetermined(why),
    };
    let mut best = LockStatus::None;
    for path in files {
        let this = status_from_path(&path);
        best = match (&best, &this) {
            (LockStatus::Active(_), _) => best,
            (_, LockStatus::Active(_)) => this,
            (LockStatus::Undetermined(_), _) => best,
            (_, LockStatus::Undetermined(_)) => this,
            (LockStatus::Stale(_), _) => best,
            (_, LockStatus::Stale(_)) => this,
            _ => best,
        };
    }
    best
}

/// Return whether any project's driver is active, using the default lock path.
pub fn status_any() -> LockStatus {
    status_any_at(None)
}

/// One progress signal's readout for the [`ProbeReport`].
#[derive(Debug, Clone, Serialize)]
pub struct SignalReport {
    /// Signal name (`git-head`, `transcript`).
    pub name: String,
    /// Whether the signal could be read this sample. An unreadable signal forces
    /// the verdict to `undetermined` (never a reap).
    pub readable: bool,
    /// The opaque value when readable (a git SHA, a `size:mtime`), or the reason
    /// it could not be read.
    pub detail: String,
}

/// The `backlog lock probe` readout: the progress verdict for the current
/// holder, the signals it was derived from, and the relevant ages. `verdict` is
/// one of `none` (no holder), `progressing`, `stalled`, or `undetermined`;
/// `reap_eligible` is true ONLY for `stalled` — the sole state in which a
/// non-`--force` acquire will reap this holder.
#[derive(Debug, Clone, Serialize)]
pub struct ProbeReport {
    pub project: String,
    pub verdict: String,
    pub reap_eligible: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub holder_session: Option<String>,
    /// Seconds since the holder's last heartbeat (`None` when there is no holder).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub heartbeat_age_secs: Option<i64>,
    /// Whether the heartbeat is past the stale TTL. NOTE: `heartbeat_stale &&
    /// !reap_eligible` is the exact case the fix protects — stale heartbeat, but
    /// progress not confirmed stalled, so the holder is NOT reaped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub heartbeat_stale: Option<bool>,
    pub signals: Vec<SignalReport>,
}

fn signal_report(name: &str, d: &Determination<Vec<u8>>) -> SignalReport {
    match d {
        Determination::Known(v) => SignalReport {
            name: name.to_string(),
            readable: true,
            detail: String::from_utf8_lossy(v).to_string(),
        },
        Determination::Undetermined(why) => SignalReport {
            name: name.to_string(),
            readable: false,
            detail: why.as_str().to_string(),
        },
    }
}

/// Probe the current holder's PROGRESS (not mere liveness) for `project`,
/// advancing the multi-sample state machine by one step and reporting the
/// verdict + the signals it was read from + ages. This is the observability
/// window onto the reap decision: `reap_eligible == true` (verdict `stalled`)
/// means a plain `acquire` would now reap this holder; `progressing` /
/// `undetermined` mean it would be protected. Repeated probes are how a genuine
/// stall accrues toward `stalled` across the window. `lock_dir` overrides the
/// directory for tests.
pub fn probe_at(project: &str, lock_dir: Option<&Path>) -> ProbeReport {
    let now = now_unix();
    let path = match lock_dir {
        Some(d) => lock_path_for(d, project),
        None => lock_path(project),
    };
    let info = match read_lock(&path) {
        Some(info) => info,
        None => {
            let (verdict, reason) = if path.exists() {
                (
                    "undetermined",
                    Some("lock file present but unreadable".to_string()),
                )
            } else {
                ("none", Some("no lock held for this project".to_string()))
            };
            return ProbeReport {
                project: project.to_string(),
                verdict: verdict.to_string(),
                reap_eligible: false,
                reason,
                holder_session: None,
                heartbeat_age_secs: None,
                heartbeat_stale: None,
                signals: Vec::new(),
            };
        }
    };

    let head = progress::git_head_signal(Path::new(&info.project));
    let transcript = progress::session_transcript_signal(&info.session_id);
    let signals = vec![
        signal_report("git-head", &head),
        signal_report("transcript", &transcript),
    ];
    let current =
        progress::fingerprint_from_signals(vec![("git-head", head), ("transcript", transcript)]);
    let key = format!("lock:{}:{}", project_slug(&info.project), info.session_id);
    let verdict = progress::sample(
        &progress_store_dir(lock_dir),
        &key,
        current,
        now,
        progress::window_secs(progress::DEFAULT_WINDOW_SECS),
    );
    let (v, reason, reap) = match &verdict {
        Determination::Known(Liveness::Progressing) => ("progressing", None, false),
        Determination::Known(Liveness::Stalled) => ("stalled", None, true),
        Determination::Undetermined(why) => ("undetermined", Some(why.as_str().to_string()), false),
    };
    ProbeReport {
        project: project.to_string(),
        verdict: v.to_string(),
        reap_eligible: reap,
        reason,
        holder_session: Some(info.session_id.clone()),
        heartbeat_age_secs: Some(now.saturating_sub(info.heartbeat_at)),
        heartbeat_stale: Some(is_stale(&info, now)),
        signals,
    }
}

/// Probe using the default lock path. See [`probe_at`].
pub fn probe(project: &str) -> ProbeReport {
    probe_at(project, None)
}

/// Refresh the heartbeat of the current lock, but only if it is held by
/// `session_id` — a session must never resurrect or extend a lock it doesn't
/// hold. Fail-soft: if there is no lock, or it is held by a different
/// session, this is a no-op (`Ok(())`) rather than an error, since a
/// heartbeat call racing a release/steal is expected, not exceptional.
/// `lock_dir` allows tests to override the directory.
pub fn heartbeat_at(session_id: &str, project: &str, lock_dir: Option<&Path>) -> Result<()> {
    let path = match lock_dir {
        Some(d) => lock_path_for(d, project),
        None => lock_path(project),
    };
    let Some(mut info) = read_lock(&path) else {
        return Ok(());
    };
    if info.session_id != session_id {
        return Ok(());
    }
    info.heartbeat_at = now_unix();
    let json = serde_json::to_string_pretty(&info)?;

    // Publish the refreshed contents atomically (temp file + rename) so a
    // concurrent reader never observes a partially-written heartbeat.
    let tmp_path = path.with_extension(format!("tmp.{}.{}", std::process::id(), now_unix_nanos()));
    {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)
            .with_context(|| format!("create temp lock file {}", tmp_path.display()))?;
        f.write_all(json.as_bytes())
            .with_context(|| format!("write temp lock file {}", tmp_path.display()))?;
        f.sync_all().ok();
    }
    let _guard = TmpGuard(&tmp_path);
    std::fs::rename(&tmp_path, &path)
        .with_context(|| format!("rename heartbeat lock file {}", path.display()))?;
    Ok(())
}

/// Refresh the heartbeat using the default lock path. See [`heartbeat_at`].
pub fn heartbeat(session_id: &str, project: &str) -> Result<()> {
    heartbeat_at(session_id, project, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn tmp() -> TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        // Tests that write a LockInfo directly via std::fs::write (bypassing
        // acquire_inner, which lazily creates this dir) need it to pre-exist.
        std::fs::create_dir_all(locks_dir_for(dir.path())).expect("create locks dir");
        dir
    }

    #[test]
    fn fresh_heartbeat_blocks_second_acquire_even_when_recorded_pid_is_already_dead() {
        // Regression: LockInfo.pid is always the one-shot acquiring CLI's own
        // pid, which is dead again by the time any later command reads the
        // lock (the process exits right after this command returns) —
        // whether or not the session that holds it is still working. Before
        // this fix, staleness was judged by `pid_alive(existing.pid)`, so
        // this pid is *always* seen as dead and the lock is reaped and stolen
        // immediately: a fresh lock offered zero real protection. Staleness
        // must instead be judged by `heartbeat_at`, which is fresh here.
        let dead_pid: u32 = 99_999_999;
        let dir = tmp();
        let d = dir.path();
        acquire_at("first", dead_pid, "proj", Some(d)).expect("first acquire");

        let second = acquire_at("second", dead_pid, "proj", Some(d));
        assert!(
            second.is_err(),
            "a lock with a fresh heartbeat must not be stealable even if its recorded pid is dead"
        );
        match status_at("proj", Some(d)) {
            LockStatus::Active(i) => assert_eq!(i.session_id, "first"),
            other => panic!("expected Active held by 'first', got {other:?}"),
        }
    }

    // NEW CONTRACT: a stale heartbeat alone no longer reaps. A stale-heartbeat
    // lock is reaped by a plain acquire ONLY when the holder's progress is
    // CONFIRMED stalled (git HEAD + transcript frozen across the window). The
    // old test asserted heartbeat-stale ⇒ reap unconditionally; that premise is
    // exactly the fail-open this change closes (a live-but-quiet holder would be
    // force-stolen). Here we force the confirmed-stalled verdict so the reap is
    // authorized, and assert the takeover still works.
    #[test]
    fn stale_heartbeat_lock_with_confirmed_stalled_progress_is_reaped() {
        let dir = tmp();
        let d = dir.path();

        let info = LockInfo {
            session_id: "old".to_string(),
            pid: 424_242,
            project: "proj".to_string(),
            acquired_at: 0,
            heartbeat_at: 0, // far older than LOCK_STALE_TTL_SECS
        };
        std::fs::write(
            lock_path_for(d, "proj"),
            serde_json::to_string(&info).unwrap(),
        )
        .unwrap();

        match status_at("proj", Some(d)) {
            LockStatus::Stale(i) => assert_eq!(i.session_id, "old"),
            other => panic!("expected Stale, got {other:?}"),
        }

        let pid = std::process::id();
        test_hook::with_forced(Determination::Known(Liveness::Stalled), || {
            acquire_at("new-sess", pid, "proj", Some(d))
                .expect("a confirmed-stalled holder must be reapable")
        });
        match status_at("proj", Some(d)) {
            LockStatus::Active(i) => assert_eq!(i.session_id, "new-sess"),
            other => panic!("expected Active, got {other:?}"),
        }
    }

    // The protective half of the new contract: a stale heartbeat whose holder is
    // still PROGRESSING must NOT be reaped by a plain acquire (this is the memory
    // scar — force-stealing a live session). Undetermined is likewise protective.
    #[test]
    fn stale_heartbeat_but_progressing_holder_is_not_reaped() {
        let dir = tmp();
        let d = dir.path();
        let info = LockInfo {
            session_id: "live-but-quiet".to_string(),
            pid: 424_242,
            project: "proj".to_string(),
            acquired_at: 0,
            heartbeat_at: 0, // stale heartbeat...
        };
        std::fs::write(
            lock_path_for(d, "proj"),
            serde_json::to_string(&info).unwrap(),
        )
        .unwrap();

        let pid = std::process::id();
        // ...but progress is confirmed Progressing ⇒ acquire must REFUSE.
        let res = test_hook::with_forced(Determination::Known(Liveness::Progressing), || {
            acquire_at("usurper", pid, "proj", Some(d))
        });
        assert!(
            res.is_err(),
            "a progressing holder must never be reaped on a stale heartbeat"
        );
        match status_at("proj", Some(d)) {
            LockStatus::Stale(i) => assert_eq!(i.session_id, "live-but-quiet"),
            other => panic!("holder must remain, got {other:?}"),
        }
    }

    #[test]
    fn stale_heartbeat_with_undetermined_progress_is_not_reaped() {
        let dir = tmp();
        let d = dir.path();
        let info = LockInfo {
            session_id: "unknowable".to_string(),
            pid: 1,
            project: "proj".to_string(),
            acquired_at: 0,
            heartbeat_at: 0,
        };
        std::fs::write(
            lock_path_for(d, "proj"),
            serde_json::to_string(&info).unwrap(),
        )
        .unwrap();

        let pid = std::process::id();
        let res =
            test_hook::with_forced(Determination::undetermined("cannot read signals"), || {
                acquire_at("usurper", pid, "proj", Some(d))
            });
        assert!(
            res.is_err(),
            "undetermined progress must fail closed: never reap"
        );
    }

    // --force still reaps a stale holder regardless of progress verdict (human
    // override), even when progress would otherwise protect it.
    #[test]
    fn force_reaps_stale_holder_even_when_progressing() {
        let dir = tmp();
        let d = dir.path();
        let info = LockInfo {
            session_id: "incumbent".to_string(),
            pid: 1,
            project: "proj".to_string(),
            acquired_at: 0,
            heartbeat_at: 0,
        };
        std::fs::write(
            lock_path_for(d, "proj"),
            serde_json::to_string(&info).unwrap(),
        )
        .unwrap();
        let pid = std::process::id();
        test_hook::with_forced(Determination::Known(Liveness::Progressing), || {
            acquire_forced_at("usurper", pid, "proj", Some(d))
                .expect("--force overrides progress protection")
        });
        match status_at("proj", Some(d)) {
            LockStatus::Active(i) => assert_eq!(i.session_id, "usurper"),
            other => panic!("force must take over, got {other:?}"),
        }
    }

    #[test]
    fn heartbeat_refreshes_only_when_held_by_caller_session() {
        let dir = tmp();
        let d = dir.path();
        let pid = std::process::id();
        acquire_at("owner", pid, "proj", Some(d)).expect("acquire");

        // Wrong session: no-op, must not touch the lock.
        heartbeat_at("someone-else", "proj", Some(d)).expect("heartbeat no-op for wrong session");
        match status_at("proj", Some(d)) {
            LockStatus::Active(i) => assert_eq!(i.session_id, "owner"),
            other => panic!("expected Active held by 'owner', got {other:?}"),
        }

        // Back-date the heartbeat directly, then confirm the owner's session
        // heartbeat call moves it forward again.
        let backdated = now_unix() - (LOCK_STALE_TTL_SECS - 1);
        {
            let mut info = read_lock(&lock_path_for(d, "proj")).expect("lock present");
            info.heartbeat_at = backdated;
            std::fs::write(
                lock_path_for(d, "proj"),
                serde_json::to_string(&info).unwrap(),
            )
            .unwrap();
        }
        heartbeat_at("owner", "proj", Some(d)).expect("heartbeat for owner session");
        let refreshed = read_lock(&lock_path_for(d, "proj")).expect("lock present");
        assert!(
            refreshed.heartbeat_at > backdated,
            "heartbeat_at should be refreshed forward by the owning session"
        );
    }

    #[test]
    fn acquire_status_release_cycle() {
        let dir = tmp();
        let d = dir.path();
        let pid = std::process::id(); // current process — definitely alive

        // Initially no lock.
        assert!(matches!(status_at("my-project", Some(d)), LockStatus::None));

        // Acquire.
        acquire_at("sess-1", pid, "my-project", Some(d)).expect("acquire");

        // Status should be Active.
        match status_at("my-project", Some(d)) {
            LockStatus::Active(info) => {
                assert_eq!(info.session_id, "sess-1");
                assert_eq!(info.pid, pid);
                assert_eq!(info.project, "my-project");
            }
            other => panic!("expected Active, got {other:?}"),
        }

        // Release.
        release_at("my-project", Some(d)).expect("release");

        // Status should be None again.
        assert!(matches!(status_at("my-project", Some(d)), LockStatus::None));
    }

    #[test]
    fn stale_detection() {
        let dir = tmp();
        let d = dir.path();

        // A lockfile whose heartbeat is far older than LOCK_STALE_TTL_SECS
        // reads as Stale regardless of the (unused) pid value.
        let info = LockInfo {
            session_id: "stale-sess".to_string(),
            pid: 99_999_999,
            project: "some-project".to_string(),
            acquired_at: 0,
            heartbeat_at: 0,
        };
        std::fs::write(
            lock_path_for(d, "some-project"),
            serde_json::to_string(&info).unwrap(),
        )
        .unwrap();

        match status_at("some-project", Some(d)) {
            LockStatus::Stale(i) => {
                assert_eq!(i.session_id, "stale-sess");
            }
            other => panic!("expected Stale, got {other:?}"),
        }
    }

    #[test]
    fn acquire_fails_when_active_lock_exists_for_same_project() {
        let dir = tmp();
        let d = dir.path();
        let pid = std::process::id();

        acquire_at("sess-a", pid, "proj-a", Some(d)).expect("first acquire");

        // Second acquire for the SAME project should fail because the
        // heartbeat is fresh.
        let err = acquire_at("sess-b", pid, "proj-a", Some(d));
        assert!(err.is_err(), "expected error acquiring locked resource");
    }

    // Core fix: the backlog lock is per-project. Two sessions racing on
    // DIFFERENT projects must never block each other — this is exactly the
    // observed friction ("/flow" on `harness` stood down because a concurrent
    // session held the lock for the unrelated `ai-aegis` project) that this
    // scoping fix exists to eliminate.
    #[test]
    fn cross_project_acquire_does_not_conflict() {
        let dir = tmp();
        let d = dir.path();
        let pid = std::process::id();

        acquire_at("sess-a", pid, "project-a", Some(d)).expect("project-a acquires");
        acquire_at("sess-b", pid, "project-b", Some(d))
            .expect("project-b must acquire concurrently without conflict");

        match status_at("project-a", Some(d)) {
            LockStatus::Active(i) => assert_eq!(i.session_id, "sess-a"),
            other => panic!("expected project-a Active held by 'sess-a', got {other:?}"),
        }
        match status_at("project-b", Some(d)) {
            LockStatus::Active(i) => assert_eq!(i.session_id, "sess-b"),
            other => panic!("expected project-b Active held by 'sess-b', got {other:?}"),
        }
    }

    #[test]
    fn acquire_overwrites_stale_lock() {
        let dir = tmp();
        let d = dir.path();

        let info = LockInfo {
            session_id: "old".to_string(),
            pid: 424_242,
            project: "proj".to_string(),
            acquired_at: 0,
            heartbeat_at: 0, // far older than LOCK_STALE_TTL_SECS
        };
        std::fs::write(
            lock_path_for(d, "proj"),
            serde_json::to_string(&info).unwrap(),
        )
        .unwrap();

        let pid = std::process::id();
        // Confirmed-stalled holder ⇒ reap authorized (new contract).
        test_hook::with_forced(Determination::Known(Liveness::Stalled), || {
            acquire_at("new-sess", pid, "proj", Some(d)).expect("should succeed over stalled lock")
        });

        match status_at("proj", Some(d)) {
            LockStatus::Active(i) => assert_eq!(i.session_id, "new-sess"),
            other => panic!("expected Active, got {other:?}"),
        }
    }

    // (a) Two consecutive acquires without a release: the second must fail.
    // With the atomic create_new path, the second acquire sees an existing lock
    // file owned by a live pid and bails — the lock cannot be double-held.
    #[test]
    fn second_acquire_without_release_fails() {
        let dir = tmp();
        let d = dir.path();
        let pid = std::process::id();

        acquire_at("first", pid, "proj", Some(d)).expect("first acquire");
        let second = acquire_at("second", pid, "proj", Some(d));
        assert!(
            second.is_err(),
            "second acquire without release must fail while lock is active"
        );

        // The original owner must still hold the lock unchanged.
        match status_at("proj", Some(d)) {
            LockStatus::Active(i) => assert_eq!(i.session_id, "first"),
            other => panic!("expected Active held by 'first', got {other:?}"),
        }
    }

    // (b) A stale lock (heartbeat past TTL) must be reaped so a fresh acquire wins.
    #[test]
    fn acquire_steals_stale_lock() {
        let dir = tmp();
        let d = dir.path();

        let info = LockInfo {
            session_id: "dead".to_string(),
            pid: 99_999_999,
            project: "proj".to_string(),
            acquired_at: 0,
            heartbeat_at: 0, // far older than LOCK_STALE_TTL_SECS
        };
        std::fs::write(
            lock_path_for(d, "proj"),
            serde_json::to_string(&info).unwrap(),
        )
        .unwrap();

        let pid = std::process::id();
        // Confirmed-stalled holder ⇒ reap authorized (new contract).
        test_hook::with_forced(Determination::Known(Liveness::Stalled), || {
            acquire_at("live", pid, "proj", Some(d)).expect("acquire must steal a stalled lock")
        });

        match status_at("proj", Some(d)) {
            LockStatus::Active(i) => {
                assert_eq!(i.session_id, "live");
                assert_eq!(i.pid, pid);
            }
            other => panic!("expected Active held by 'live', got {other:?}"),
        }
    }

    // --force steals a lock with a fresh heartbeat, where a plain acquire
    // would (correctly) fail. This is the documented 強制奪取 escape hatch.
    #[test]
    fn force_acquire_steals_a_live_lock() {
        let dir = tmp();
        let d = dir.path();
        let live_pid = std::process::id(); // holder with a fresh heartbeat

        acquire_at("incumbent", live_pid, "proj", Some(d)).expect("incumbent acquires");

        // Plain acquire must refuse a live holder.
        assert!(
            acquire_at("usurper", live_pid, "proj", Some(d)).is_err(),
            "plain acquire must not steal a live lock"
        );

        // Forced acquire takes it over.
        acquire_forced_at("usurper", live_pid, "proj", Some(d))
            .expect("--force must steal a live lock");

        match status_at("proj", Some(d)) {
            LockStatus::Active(i) => {
                assert_eq!(i.session_id, "usurper");
                assert_eq!(i.project, "proj");
            }
            other => panic!("expected the usurper's lock active, got {other:?}"),
        }
    }

    // --force on an *unheld* lock behaves like a normal acquire (no existing
    // file to displace), so the escape hatch is always safe to pass.
    #[test]
    fn force_acquire_on_free_lock_just_acquires() {
        let dir = tmp();
        let d = dir.path();
        let pid = std::process::id();
        assert!(matches!(status_at("proj", Some(d)), LockStatus::None));
        acquire_forced_at("solo", pid, "proj", Some(d)).expect("force on free lock acquires");
        match status_at("proj", Some(d)) {
            LockStatus::Active(i) => assert_eq!(i.session_id, "solo"),
            other => panic!("expected Active, got {other:?}"),
        }
    }

    // (c) Concurrency stand-in: a lock file already exists on disk at the moment
    // acquire runs (as if a competitor created it just before us). If the owner
    // is active, acquire must fail; if the owner is stale, acquire must succeed.
    #[test]
    fn acquire_against_preexisting_lock_file() {
        // Active owner present -> must fail.
        {
            let dir = tmp();
            let d = dir.path();
            let live_pid = std::process::id();
            let existing = LockInfo {
                session_id: "competitor".to_string(),
                pid: live_pid,
                project: "our-proj".to_string(),
                acquired_at: now_unix(),
                heartbeat_at: now_unix(),
            };
            std::fs::write(
                lock_path_for(d, "our-proj"),
                serde_json::to_string(&existing).unwrap(),
            )
            .unwrap();

            let res = acquire_at("us", live_pid, "our-proj", Some(d));
            assert!(
                res.is_err(),
                "acquire must fail when an active lock file already exists"
            );
            match status_at("our-proj", Some(d)) {
                LockStatus::Active(i) => assert_eq!(i.session_id, "competitor"),
                other => panic!("expected the competitor's lock intact, got {other:?}"),
            }
        }

        // Stale owner present -> must succeed and take over.
        {
            let dir = tmp();
            let d = dir.path();
            let existing = LockInfo {
                session_id: "ghost".to_string(),
                pid: 99_999_999,
                project: "our-proj".to_string(),
                acquired_at: 0,
                heartbeat_at: 0, // far older than LOCK_STALE_TTL_SECS
            };
            std::fs::write(
                lock_path_for(d, "our-proj"),
                serde_json::to_string(&existing).unwrap(),
            )
            .unwrap();

            let our_pid = std::process::id();
            // Confirmed-stalled holder ⇒ reap authorized (new contract).
            test_hook::with_forced(Determination::Known(Liveness::Stalled), || {
                acquire_at("us", our_pid, "our-proj", Some(d))
                    .expect("acquire must succeed over a stalled pre-existing lock file")
            });
            match status_at("our-proj", Some(d)) {
                LockStatus::Active(i) => assert_eq!(i.session_id, "us"),
                other => panic!("expected our lock active, got {other:?}"),
            }
        }
    }

    // Direct TOCTOU regression: many threads race to acquire the same empty
    // lock. With the old check-then-write code, multiple threads observed "no
    // lock" and all wrote, so >1 acquire succeeded (double acquisition). With
    // the atomic create_new path exactly one wins. All pids are the live
    // current process, so a winner is never reaped as stale.
    #[test]
    fn concurrent_acquire_admits_exactly_one() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::{Arc, Barrier};

        let dir = tmp();
        let d = Arc::new(dir.path().to_path_buf());
        let pid = std::process::id();

        const N: usize = 16;
        let barrier = Arc::new(Barrier::new(N));
        let winners = Arc::new(AtomicUsize::new(0));

        let mut handles = Vec::with_capacity(N);
        for i in 0..N {
            let d = Arc::clone(&d);
            let barrier = Arc::clone(&barrier);
            let winners = Arc::clone(&winners);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                if acquire_at(&format!("sess-{i}"), pid, "proj", Some(d.as_path())).is_ok() {
                    winners.fetch_add(1, Ordering::SeqCst);
                }
            }));
        }
        for h in handles {
            h.join().expect("thread join");
        }

        assert_eq!(
            winners.load(Ordering::SeqCst),
            1,
            "exactly one concurrent acquire must succeed (no double acquisition)"
        );
    }

    #[test]
    fn status_any_is_none_when_locks_dir_empty_or_missing() {
        let dir = tmp();
        let d = dir.path();
        assert!(matches!(status_any_at(Some(d)), LockStatus::None));

        // Even a missing locks/ dir (never created) reads as None, not an error.
        let missing = dir.path().join("does-not-exist");
        assert!(matches!(status_any_at(Some(&missing)), LockStatus::None));
    }

    // This is the behavior `daily::driver_active()` depends on: a bare
    // `backlog lock status` (no --project) must still report "someone is
    // active" for ANY project's lock, not just one fixed project.
    #[test]
    fn status_any_finds_active_lock_regardless_of_which_project_holds_it() {
        let dir = tmp();
        let d = dir.path();
        let pid = std::process::id();
        acquire_at("driver-sess", pid, "some-other-project", Some(d)).expect("acquire");

        match status_any_at(Some(d)) {
            LockStatus::Active(i) => assert_eq!(i.session_id, "driver-sess"),
            other => panic!("expected Active from some-other-project, got {other:?}"),
        }
    }

    // §3: "there is a lock file but I cannot read it" is not "there is no
    // lock". The old code mapped both to `None`, i.e. "free" — the permissive
    // answer to a question that was never answered.
    #[test]
    fn unreadable_lock_file_is_undetermined_not_none() {
        let dir = tmp();
        let d = dir.path();
        std::fs::write(lock_path_for(d, "proj"), "{ not json").unwrap();
        match status_at("proj", Some(d)) {
            LockStatus::Undetermined(_) => {}
            other => panic!("expected Undetermined for a corrupt lock file, got {other:?}"),
        }
        // ...and the cross-project scan must not launder it into None either.
        match status_any_at(Some(d)) {
            LockStatus::Undetermined(_) => {}
            other => panic!("expected Undetermined from the any-project scan, got {other:?}"),
        }
    }

    // An Active lock elsewhere already answers "a driver is active", so it
    // outranks Undetermined; Undetermined still outranks Stale/None, which both
    // read downstream as "not active".
    #[test]
    fn status_any_ranks_active_over_undetermined_over_stale() {
        let dir = tmp();
        let d = dir.path();
        std::fs::write(lock_path_for(d, "corrupt"), "{ not json").unwrap();
        let stale = LockInfo {
            session_id: "stale-sess".to_string(),
            pid: 1,
            project: "proj-stale".to_string(),
            acquired_at: 0,
            heartbeat_at: 0,
        };
        std::fs::write(
            lock_path_for(d, "proj-stale"),
            serde_json::to_string(&stale).unwrap(),
        )
        .unwrap();
        match status_any_at(Some(d)) {
            LockStatus::Undetermined(_) => {}
            other => panic!("undetermined must outrank stale, got {other:?}"),
        }

        acquire_at("live-sess", std::process::id(), "proj-live", Some(d)).unwrap();
        match status_any_at(Some(d)) {
            LockStatus::Active(i) => assert_eq!(i.session_id, "live-sess"),
            other => panic!("an active lock must outrank undetermined, got {other:?}"),
        }
    }

    #[test]
    fn status_any_prefers_active_over_stale_across_projects() {
        let dir = tmp();
        let d = dir.path();
        let pid = std::process::id();

        // A stale lock for one project...
        let stale = LockInfo {
            session_id: "stale-sess".to_string(),
            pid: 99_999_999,
            project: "proj-stale".to_string(),
            acquired_at: 0,
            heartbeat_at: 0,
        };
        std::fs::write(
            lock_path_for(d, "proj-stale"),
            serde_json::to_string(&stale).unwrap(),
        )
        .unwrap();

        // ...and a live lock for a different project.
        acquire_at("live-sess", pid, "proj-live", Some(d)).expect("acquire");

        match status_any_at(Some(d)) {
            LockStatus::Active(i) => assert_eq!(i.session_id, "live-sess"),
            other => panic!("expected the Active (not Stale) lock to win, got {other:?}"),
        }
    }

    // --- probe -------------------------------------------------------------

    #[test]
    fn probe_reports_none_when_no_lock() {
        let dir = tmp();
        let d = dir.path();
        let r = probe_at("proj", Some(d));
        assert_eq!(r.verdict, "none");
        assert!(!r.reap_eligible);
        assert!(r.holder_session.is_none());
        assert!(r.signals.is_empty());
    }

    // A held lock in a bare temp dir (not a git repo, no discoverable
    // transcript) has both signals UNREADABLE ⇒ the verdict is `undetermined`
    // and it is NOT reap-eligible — a probe must never call a holder reapable
    // off signals it could not even read.
    #[test]
    fn probe_of_holder_with_unreadable_signals_is_undetermined_not_reapable() {
        let dir = tmp();
        let d = dir.path();
        let info = LockInfo {
            session_id: "held".to_string(),
            pid: 1,
            project: d.join("not-a-repo").to_string_lossy().to_string(),
            acquired_at: 0,
            heartbeat_at: 0,
        };
        std::fs::write(
            lock_path_for(d, "proj"),
            serde_json::to_string(&info).unwrap(),
        )
        .unwrap();
        let r = probe_at("proj", Some(d));
        assert_eq!(r.verdict, "undetermined", "got {r:?}");
        assert!(!r.reap_eligible, "undetermined must never be reap-eligible");
        assert_eq!(r.holder_session.as_deref(), Some("held"));
        assert_eq!(r.heartbeat_stale, Some(true));
        // Both signals reported as unreadable.
        assert!(r.signals.iter().all(|s| !s.readable), "got {:?}", r.signals);
        assert_eq!(r.signals.len(), 2);
    }
}
