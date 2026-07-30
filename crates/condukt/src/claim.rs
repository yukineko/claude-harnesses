//! Cross-session file-claim registry — stops two condukt sessions on the SAME
//! machine from processing overlapping work (PDO = Parallel Development
//! Orchestration).
//!
//! # The gap this closes
//!
//! `state::cross_run_conflicts` (Phase 3.5) is an *advisory one-time snapshot*:
//! it compares an incoming decomposition's `touched_files` against other open
//! runs at schedule time. Two sessions racing at the same instant — both before
//! either has written its run-state — never see each other, and it never
//! *enforces* a skip. The per-run [`crate::lock::RunLock`] only serializes writes
//! to the *same* run; each session has its own run, so cross-run work is not
//! serialized at all.
//!
//! This registry is the missing piece: a project-scoped JSON file that every
//! session on the machine keeps writing its live claims into, and reads before
//! executing a task. It is keyed by the file paths/globs a task touches — the one
//! identity that is meaningful *across* runs (task ids like `t1` are per-run and
//! not comparable). Overlap is decided with the same glob-aware
//! [`crate::schedule::files_conflict`] the scheduler already uses.
//!
//! # Behavior (design decisions, locked with the user)
//!
//! - **File-level lease**: a session claims the `touched_files` of the task it is
//!   about to run.
//! - **Hard skip**: if any of those files overlap with a claim held by a *live*
//!   holder from *another* run, the task is not run ("already being processed →
//!   don't process it"). Non-conflicting files are still claimed so the rest of a
//!   batch proceeds (partial progress).
//! - **Heartbeat**: a session refreshes `heartbeat_at` while it works (the "keep
//!   writing to a file" part), so a live-but-quiet holder is not reaped.
//! - **Stale reap**: a claim whose heartbeat is older than the stuck-TTL is
//!   reaped so a dead session never blocks others forever. Liveness is anchored
//!   to the heartbeat, NOT the recorded pid — the condukt CLI is ephemeral, so a
//!   pid check would read every claim as dead on the next invocation.
//!
//! # Concurrency & safety
//!
//! Every mutation is a load → mutate → save cycle guarded by the proven
//! [`crate::lock::RunLock`], keyed on the reserved id [`CLAIMS_LOCK_KEY`] so all
//! sessions serialize on one lock file (`<project>/__claims__.lock`) and no update
//! is lost. Writes are atomic (temp + rename). Everything is fail-soft: a missing
//! or corrupt registry is treated as empty rather than aborting a state
//! transition, and the module never panics.

use crate::config::Config;
use crate::lock::RunLock;
use crate::schedule::files_conflict;
use crate::store::{project_key, repo_root};
use anyhow::Result;
use harness_core::progress::{self, Liveness};
use harness_core::verdict::Determination;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Reserved run-id used only to key the registry's own RMW lock file
/// (`<project>/__claims__.lock`), reusing the proven per-run lock. It never names
/// a real run, so it cannot collide with one.
const CLAIMS_LOCK_KEY: &str = "__claims__";

/// Synthetic holder run-id stamped on a [`Skipped`] entry when the claims-registry
/// lock was contended past its deadline (rather than a real peer run holding the
/// file). Marks a fail-CLOSED skip so the caller HARD-SKIPS its task instead of
/// proceeding to an unlocked read-modify-write that could double-claim.
const LOCK_CONTENDED_HOLDER: &str = "__lock_contended__";

/// Fail-CLOSED outcome for a contended claims-registry lock: every requested
/// identity (file path or task hashkey) is reported skipped and none claimed, so
/// the caller treats them as unavailable and skips the task. This is the whole
/// point of the hard-skip lock — under pathological contention two timed-out
/// writers must NOT both proceed to an unlocked RMW and double-claim the same
/// work (last-writer-wins). The synthetic [`LOCK_CONTENDED_HOLDER`] distinguishes
/// a contention skip from a real peer-run skip.
fn all_skipped(idents: &[String]) -> ClaimOutcome {
    ClaimOutcome {
        claimed: Vec::new(),
        skipped: idents
            .iter()
            .map(|f| Skipped {
                file: f.clone(),
                holder_run: LOCK_CONTENDED_HOLDER.to_string(),
                holder_pid: 0,
                holder_session: None,
            })
            .collect(),
    }
}

/// Test-only race-window widener for [`claim_tasks`]'s load->check->save section.
/// Real process-spawn overhead dwarfs that section's natural duration, so two
/// racing `condukt state claim-task` processes essentially never interleave in
/// it by chance — a concurrency regression test needs a deterministic way to
/// force the interleave inside the [`RunLock`]-held critical section. No-op
/// unless `CONDUKT_TEST_CLAIM_DELAY_MS` is set (never set outside the
/// `run_lock_concurrency` integration test), so production behavior is
/// unchanged. Mirrors `overwatch::lease::artificial_race_delay`.
fn artificial_race_delay() {
    if let Some(ms) = std::env::var("CONDUKT_TEST_CLAIM_DELAY_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
    {
        std::thread::sleep(std::time::Duration::from_millis(ms));
    }
}

/// One live occupancy of a file path/glob by a session's run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claim {
    /// The condukt run that holds this file.
    pub run_id: String,
    /// The Claude session that owns the run (`CLAUDE_CODE_SESSION_ID`), if known.
    #[serde(default)]
    pub session_id: Option<String>,
    /// OS pid of the process that last (re)claimed/heartbeat this file — used for
    /// liveness checks and stale reaping.
    pub pid: u32,
    /// When the file was first claimed by this run (unix seconds).
    pub claimed_at: i64,
    /// Last liveness refresh (unix seconds). Bumped on every (re)claim/heartbeat.
    pub heartbeat_at: i64,
    /// Human-readable task title, for inspection / the execution-state view. Only
    /// task-level claims carry it; file-level claims leave it `None`. Kept optional
    /// (and skipped when absent) so existing file-claim JSON stays byte-identical
    /// and old registries without the field still load.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// A file that could not be claimed because a live holder from another run owns
/// an overlapping path/glob.
#[derive(Debug, Clone, Serialize)]
pub struct Skipped {
    pub file: String,
    pub holder_run: String,
    pub holder_pid: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub holder_session: Option<String>,
}

/// Result of a [`claim_files`] attempt: which files we now hold, and which were
/// skipped (held live by another run). A non-empty `skipped` means the caller
/// must NOT process the corresponding task (hard skip).
#[derive(Debug, Default, Serialize)]
pub struct ClaimOutcome {
    pub claimed: Vec<String>,
    pub skipped: Vec<Skipped>,
}

/// The on-disk registry: two parallel occupancy tables sharing one lock and one
/// RMW cycle.
///
/// - `files`: file path/glob -> its live holder (the original PDO collision guard).
///   Flattened to the top level so a pre-existing `claims.json` written as a bare
///   `{ "<path>": {..} }` map still deserializes here unchanged (backward compat).
/// - `task_claims`: task hashkey -> its live holder. Hashkeys are opaque strings
///   supplied by callers (computed elsewhere); this module never derives them.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Registry {
    /// File path/glob -> live holder. Flattened for backward-compatible on-disk
    /// layout with the original bare-map format.
    #[serde(flatten)]
    pub files: BTreeMap<String, Claim>,
    /// Task hashkey -> live holder. Absent in old registries, so defaulted.
    #[serde(default)]
    pub task_claims: BTreeMap<String, Claim>,
}

/// `<state_dir>/<project-key>/claims.json` — beside the run-state files, so it is
/// per-project and unrelated projects never share a registry.
fn registry_path(cfg: &Config, cwd: &Path) -> PathBuf {
    cfg.state_dir
        .join(project_key(&repo_root(cwd)))
        .join("claims.json")
}

/// `<state_dir>/<project-key>/execution-state.json` — the joined "who is running
/// what" view, written beside `claims.json`.
// Wired into the CLI by the follow-up task; used by tests today.
#[allow(dead_code)]
fn execution_state_path(cfg: &Config, cwd: &Path) -> PathBuf {
    cfg.state_dir
        .join(project_key(&repo_root(cwd)))
        .join("execution-state.json")
}

/// Fail-soft load: a missing or corrupt registry is treated as empty rather than
/// breaking the caller. (Corruption loses others' claims, but proceeding is safer
/// than aborting a state transition — the worst case degrades to today's
/// no-claim behavior.)
fn load(path: &Path) -> Registry {
    match std::fs::read_to_string(path) {
        Ok(txt) => serde_json::from_str(&txt).unwrap_or_default(),
        Err(_) => Registry::default(),
    }
}

/// Atomic write (temp + rename), mirroring `RunState::save`. Generic so it serves
/// both the registry and the derived execution-state view.
fn save<T: Serialize>(path: &Path, val: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(val)?;
    let tmp = unique_tmp(path, "json");
    std::fs::write(&tmp, &json)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Process-wide monotonic counter so two saves in the SAME process (identical
/// pid, and possibly an identical `now_unix_nanos()` under a coarse clock) never
/// derive the same temp name. Without it a nanos collision between two concurrent
/// degraded writers (e.g. after a fail-soft lock timeout) could have both write
/// the SAME fixed `json.tmp` path, so a rename publishes a half-written registry —
/// which loads as empty, wiping every claim and enabling mass double-claim.
/// Mirrors `lock::TMP_SEQ` / `overwatch::store::TMP_SEQ`.
static TMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn now_unix_nanos() -> u128 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// Build a per-call-unique sibling temp path for an atomic save:
/// `<stem>.<tag>.tmp.<pid>.<nanos>.<seq>`. Two concurrent saves of the same
/// registry therefore never collide on a single temp name (which would let one
/// publish a half-written file the other renames into place), mirroring
/// `lock.rs`'s `TMP_SEQ`/`now_unix_nanos` idiom.
fn unique_tmp(path: &Path, tag: &str) -> PathBuf {
    use std::sync::atomic::Ordering;
    path.with_extension(format!(
        "{tag}.tmp.{}.{}.{}",
        std::process::id(),
        now_unix_nanos(),
        TMP_SEQ.fetch_add(1, Ordering::Relaxed)
    ))
}

/// A claim's heartbeat is stale when it is older than the TTL — the session that
/// held it went QUIET (died, or stalled past the stuck-TTL) without releasing.
///
/// Heartbeat-staleness is NECESSARY but NOT SUFFICIENT to reap: a quiet session
/// may still be doing durable work (committing, growing its transcript, advancing
/// tasks). The actual reap ([`reap`]/[`retain_claim`]) additionally requires a
/// confirmed `Known(Stalled)` PROGRESS verdict ([`claim_progress`]) before it
/// steals the claim; a stale-but-progressing (or unconfirmable) holder is kept.
///
/// Staleness is anchored to the **heartbeat**, NOT to `c.pid`: the condukt CLI is
/// ephemeral (each invocation is a separate short-lived process that exits
/// immediately), so the recorded pid is essentially always dead by the next
/// command. Reaping on pid-liveness would therefore wipe every claim on the very
/// next invocation. A live session instead proves liveness by refreshing the
/// heartbeat (via `state set`/`state heartbeat`); `c.pid` is retained only for
/// observability (which condukt process last touched the claim). The TTL reuses
/// the STUCK TTL, so a claim becomes ELIGIBLE for a progress-gated reap exactly
/// when its task would already be considered stuck.
fn is_stale(c: &Claim, now: i64, ttl: i64) -> bool {
    now.saturating_sub(c.heartbeat_at) > ttl
}

/// Directory where per-run progress snapshots are persisted (one file per run,
/// keyed by run id). Colocated with the claim registry's state dir so it is torn
/// down with the run's state, never leaking across projects.
pub(crate) fn progress_store_dir(cfg: &Config, cwd: &Path) -> PathBuf {
    registry_path(cfg, cwd)
        .parent()
        .map(|p| p.join("progress"))
        .unwrap_or_else(|| PathBuf::from("progress"))
}

/// Multi-signal, multi-sample PROGRESS verdict for the run owning claim `c`.
///
/// A heartbeat lapsing past the stuck-TTL proves only that the session went
/// QUIET, not that it DIED — the reap must not force-steal a holder that is
/// still doing durable work. This samples three durable signals (git HEAD of the
/// project repo, the owning session's transcript growth, and the run's max task
/// `updated_at` advancing) across the [`progress`] window and returns:
/// `Known(Stalled)` only when ALL readable signals are frozen for the full
/// window; `Known(Progressing)` when any advanced; `Undetermined` when a signal
/// is unreadable, there is no prior sample yet, or the window has not elapsed.
/// Reap fires ONLY on `Known(Stalled)` — Progressing and Undetermined both
/// preserve the claim (fail-closed).
fn claim_progress(cfg: &Config, cwd: &Path, c: &Claim, now: i64) -> Determination<Liveness> {
    #[cfg(test)]
    if let Some(forced) = test_hook::forced_progress() {
        return forced;
    }
    let head = progress::git_head_signal(&repo_root(cwd));
    let transcript = match c.session_id.as_deref() {
        Some(sid) => progress::session_transcript_signal(sid),
        None => Determination::undetermined(
            "claim has no owning session id — transcript progress unreadable",
        ),
    };
    let task_progress = run_task_progress_signal(cfg, cwd, &c.run_id);
    let current = progress::fingerprint_from_signals(vec![
        ("git-head", head),
        ("transcript", transcript),
        ("run-tasks", task_progress),
    ]);
    let key = format!("claim:{}", c.run_id);
    progress::sample(
        &progress_store_dir(cfg, cwd),
        &key,
        current,
        now,
        progress::window_secs(progress::DEFAULT_WINDOW_SECS),
    )
}

/// Durable run-progress signal: the maximum `updated_at` across the run's tasks.
///
/// This is deliberately NOT the run-state file mtime — heartbeats rewrite that
/// file without changing any task, so file mtime tracks LIVENESS, not progress.
/// `TaskState.updated_at` only advances on a real status/field transition, so
/// its max is a true "durable work advanced" signal. An unloadable run state is
/// `Undetermined` (protective — an unreadable signal must never read as frozen).
fn run_task_progress_signal(cfg: &Config, cwd: &Path, run_id: &str) -> Determination<Vec<u8>> {
    match crate::state::RunState::load(cfg, cwd, run_id) {
        Ok(rs) => {
            let max_updated = rs
                .tasks
                .iter()
                .filter_map(|t| t.updated_at)
                .max()
                .unwrap_or(0);
            Determination::Known(max_updated.to_string().into_bytes())
        }
        Err(e) => Determination::undetermined(format!("run state for {run_id} unreadable: {e}")),
    }
}

/// Decide whether a claim survives a reap. A claim with a FRESH heartbeat is
/// always kept. A claim whose heartbeat has lapsed past the TTL is kept UNLESS
/// its run's progress is confirmed `Known(Stalled)` — Progressing or
/// Undetermined preserve it (fail-closed: "cannot determine" never reaps).
fn retain_claim(
    c: &Claim,
    now: i64,
    ttl: i64,
    progress: &dyn Fn(&Claim) -> Determination<Liveness>,
) -> bool {
    if !is_stale(c, now, ttl) {
        return true;
    }
    !matches!(progress(c), Determination::Known(Liveness::Stalled))
}

/// Reap stale claims, gated on confirmed run non-progress. `progress` yields the
/// owning run's multi-sample progress verdict for a claim; a heartbeat-stale
/// claim is reaped ONLY when that verdict is `Known(Stalled)`. See
/// [`retain_claim`] and [`claim_progress`].
fn reap(
    reg: &mut Registry,
    now: i64,
    ttl: i64,
    progress: &dyn Fn(&Claim) -> Determination<Liveness>,
) {
    reg.files.retain(|_, c| retain_claim(c, now, ttl, progress));
    reg.task_claims
        .retain(|_, c| retain_claim(c, now, ttl, progress));
}

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
    /// Force [`claim_progress`] to return `v` for the duration of `body` (this
    /// thread only). Compiled out of production — the reap gate is exercised in
    /// tests without spawning git or racing wall-clock windows.
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

fn ttl_secs(cfg: &Config) -> i64 {
    // Reuse the STUCK TTL: a task silent past it is already considered stuck, so
    // its claim should likewise be reclaimable. Clamp to i64 defensively.
    cfg.stuck_ttl_secs.min(i64::MAX as u64) as i64
}

/// Claim `files` for `run_id`, hard-skipping any that overlap with a live holder
/// from another run. Reaps stale claims first. Returns the split of claimed vs
/// skipped files. The RMW is serialized by the reserved-key run lock.
pub fn claim_files(
    cfg: &Config,
    cwd: &Path,
    run_id: &str,
    session_id: Option<&str>,
    files: &[String],
    now: i64,
) -> Result<ClaimOutcome> {
    claim_files_with_deadline(cfg, cwd, run_id, session_id, files, now, RunLock::DEADLINE)
}

/// Deadline-parameterized core of [`claim_files`] (production passes the 10s
/// default; the wedged-holder regression test passes a short deadline to drive
/// the same skip-on-contention path fast). HARD-SKIP semantics: if the registry
/// lock cannot be acquired within `deadline` we return an [`all_skipped`]
/// outcome and NEVER touch the registry — the old fail-soft `acquire` instead
/// handed back an UNLOCKED guard and let the RMW proceed, which is exactly the
/// double-claim window this closes.
fn claim_files_with_deadline(
    cfg: &Config,
    cwd: &Path,
    run_id: &str,
    session_id: Option<&str>,
    files: &[String],
    now: i64,
    deadline: std::time::Duration,
) -> Result<ClaimOutcome> {
    let ttl = ttl_secs(cfg);
    let path = registry_path(cfg, cwd);
    let _lock = match RunLock::acquire_or_skip_with_deadline(cfg, cwd, CLAIMS_LOCK_KEY, deadline) {
        Some(l) => l,
        None => return Ok(all_skipped(files)),
    };
    let mut reg = load(&path);
    reap(&mut reg, now, ttl, &|c| claim_progress(cfg, cwd, c, now));

    let mut outcome = ClaimOutcome::default();
    for f in files {
        // Does any *other* run hold an overlapping claim? Clone the holder out so
        // the immutable borrow ends before we mutate `reg` below.
        let conflict = reg
            .files
            .iter()
            .find(|(k, c)| {
                c.run_id != run_id
                    && files_conflict(std::slice::from_ref(f), std::slice::from_ref(*k))
            })
            .map(|(_, c)| c.clone());

        if let Some(holder) = conflict {
            outcome.skipped.push(Skipped {
                file: f.clone(),
                holder_run: holder.run_id,
                holder_pid: holder.pid,
                holder_session: holder.session_id,
            });
            continue;
        }

        // Free, or already ours: (re)claim and refresh the heartbeat. Preserve the
        // original claimed_at if we already held this file.
        let claimed_at = reg.files.get(f).map(|c| c.claimed_at).unwrap_or(now);
        reg.files.insert(
            f.clone(),
            Claim {
                run_id: run_id.to_string(),
                session_id: session_id.map(str::to_string),
                pid: std::process::id(),
                claimed_at,
                heartbeat_at: now,
                title: None,
            },
        );
        outcome.claimed.push(f.clone());
    }
    save(&path, &reg)?;
    Ok(outcome)
}

/// Claim `hashkeys` (opaque per-task identities computed by the caller) for
/// `run_id`, hard-skipping any already held live by a *different* run. Symmetric to
/// [`claim_files`] but keyed by exact hashkey rather than glob-aware file overlap,
/// and running in the SAME reserved-key lock / RMW cycle. Reaps stale claims first.
/// `title` is stored for the execution-state view; when `None` on a re-claim the
/// previously recorded title is preserved. Returns the split of claimed vs skipped
/// hashkeys (in [`ClaimOutcome`], where `Skipped::file` carries the hashkey).
// Wired into the CLI by the follow-up task.
#[allow(dead_code)]
pub fn claim_tasks(
    cfg: &Config,
    cwd: &Path,
    hashkeys: &[String],
    run_id: &str,
    session_id: Option<&str>,
    now: i64,
    title: Option<&str>,
) -> Result<ClaimOutcome> {
    let ttl = ttl_secs(cfg);
    let path = registry_path(cfg, cwd);
    // HARD-SKIP on contention: proceeding to an unlocked RMW here is what lets
    // two timed-out writers both claim the same hashkey (double-claim). Treat
    // every requested hashkey as unavailable so the caller skips the task.
    let _lock = match RunLock::acquire_or_skip(cfg, cwd, CLAIMS_LOCK_KEY) {
        Some(l) => l,
        None => return Ok(all_skipped(hashkeys)),
    };
    let mut reg = load(&path);
    reap(&mut reg, now, ttl, &|c| claim_progress(cfg, cwd, c, now));
    artificial_race_delay();

    let mut outcome = ClaimOutcome::default();
    for hk in hashkeys {
        // After reaping, any remaining holder is live. A holder from another run is
        // a hard skip; our own is a refresh.
        if let Some(holder) = reg.task_claims.get(hk) {
            if holder.run_id != run_id {
                outcome.skipped.push(Skipped {
                    file: hk.clone(),
                    holder_run: holder.run_id.clone(),
                    holder_pid: holder.pid,
                    holder_session: holder.session_id.clone(),
                });
                continue;
            }
        }

        // Free, or already ours: (re)claim and refresh. Preserve the original
        // claimed_at and any prior title when the caller passes none.
        let (claimed_at, prior_title) = reg
            .task_claims
            .get(hk)
            .map(|c| (c.claimed_at, c.title.clone()))
            .unwrap_or((now, None));
        reg.task_claims.insert(
            hk.clone(),
            Claim {
                run_id: run_id.to_string(),
                session_id: session_id.map(str::to_string),
                pid: std::process::id(),
                claimed_at,
                heartbeat_at: now,
                title: title.map(str::to_string).or(prior_title),
            },
        );
        outcome.claimed.push(hk.clone());
    }
    save(&path, &reg)?;
    Ok(outcome)
}

/// Release every claim (files AND tasks) held by `run_id` (call on run completion
/// / gate cleanup). Returns the total number of claims released.
pub fn release_run(cfg: &Config, cwd: &Path, run_id: &str) -> Result<usize> {
    let path = registry_path(cfg, cwd);
    // On contention, SKIP (return 0 released) rather than run an unlocked RMW
    // that could clobber a concurrent writer's fresh claims. Safe: an unreleased
    // claim ages out via the heartbeat-TTL stale reap.
    let _lock = match RunLock::acquire_or_skip(cfg, cwd, CLAIMS_LOCK_KEY) {
        Some(l) => l,
        None => return Ok(0),
    };
    let mut reg = load(&path);
    let before = reg.files.len() + reg.task_claims.len();
    reg.files.retain(|_, c| c.run_id != run_id);
    reg.task_claims.retain(|_, c| c.run_id != run_id);
    let removed = before - reg.files.len() - reg.task_claims.len();
    save(&path, &reg)?;
    Ok(removed)
}

/// Release specific `files` held by `run_id` (call when a task reaches a terminal
/// status). Only removes files this run actually holds. Returns the count removed.
pub fn release_files(cfg: &Config, cwd: &Path, run_id: &str, files: &[String]) -> Result<usize> {
    let path = registry_path(cfg, cwd);
    // On contention, SKIP (0 released) rather than run an unlocked RMW that
    // could clobber concurrent claims. Safe: a stale claim ages out via TTL.
    let _lock = match RunLock::acquire_or_skip(cfg, cwd, CLAIMS_LOCK_KEY) {
        Some(l) => l,
        None => return Ok(0),
    };
    let mut reg = load(&path);
    let before = reg.files.len();
    reg.files
        .retain(|k, c| !(c.run_id == run_id && files.contains(k)));
    let removed = before - reg.files.len();
    save(&path, &reg)?;
    Ok(removed)
}

/// Release specific task `hashkeys` from the task-claim table (call when a task
/// reaches a terminal status). Removes the entries regardless of which run holds
/// them — the caller owns hashkey identity. Returns the count removed.
// Wired into the CLI by the follow-up task.
#[allow(dead_code)]
pub fn release_tasks(cfg: &Config, cwd: &Path, hashkeys: &[String]) -> Result<usize> {
    let path = registry_path(cfg, cwd);
    // On contention, SKIP (0 released) rather than run an unlocked RMW that
    // could clobber concurrent claims. Safe: a stale claim ages out via TTL.
    let _lock = match RunLock::acquire_or_skip(cfg, cwd, CLAIMS_LOCK_KEY) {
        Some(l) => l,
        None => return Ok(0),
    };
    let mut reg = load(&path);
    let before = reg.task_claims.len();
    reg.task_claims.retain(|k, _| !hashkeys.contains(k));
    let removed = before - reg.task_claims.len();
    save(&path, &reg)?;
    Ok(removed)
}

/// Refresh the heartbeat of every file held by `run_id` — the "keep writing to
/// the file" liveness signal that protects a live-but-quiet session from being
/// reaped. Reaps stale claims first. Returns how many claims were refreshed.
pub fn heartbeat(cfg: &Config, cwd: &Path, run_id: &str, now: i64) -> Result<usize> {
    let ttl = ttl_secs(cfg);
    let path = registry_path(cfg, cwd);
    // On contention, SKIP (0 refreshed) rather than run an unlocked RMW that
    // could clobber concurrent claims. A single missed heartbeat is safe — the
    // claim only ages out after the full stuck-TTL of silence.
    let _lock = match RunLock::acquire_or_skip(cfg, cwd, CLAIMS_LOCK_KEY) {
        Some(l) => l,
        None => return Ok(0),
    };
    let mut reg = load(&path);
    reap(&mut reg, now, ttl, &|c| claim_progress(cfg, cwd, c, now));
    let mut n = 0;
    for c in reg.files.values_mut().chain(reg.task_claims.values_mut()) {
        if c.run_id == run_id {
            c.heartbeat_at = now;
            n += 1;
        }
    }
    save(&path, &reg)?;
    Ok(n)
}

/// Return the live registry after reaping stale entries (persisted). Used by
/// `condukt state claims` for observability.
pub fn active_claims(cfg: &Config, cwd: &Path, now: i64) -> Result<Registry> {
    let ttl = ttl_secs(cfg);
    let path = registry_path(cfg, cwd);
    // Observability read. On contention, return a freshly-loaded, reaped
    // snapshot WITHOUT persisting (the save is only a housekeeping compaction) —
    // proceeding to an unlocked save could clobber a concurrent writer.
    match RunLock::acquire_or_skip(cfg, cwd, CLAIMS_LOCK_KEY) {
        Some(_lock) => {
            let mut reg = load(&path);
            reap(&mut reg, now, ttl, &|c| claim_progress(cfg, cwd, c, now));
            save(&path, &reg)?;
            Ok(reg)
        }
        None => {
            let mut reg = load(&path);
            reap(&mut reg, now, ttl, &|c| claim_progress(cfg, cwd, c, now));
            Ok(reg)
        }
    }
}

/// Three-valued liveness verdict for a run id in the claim registry — see
/// [`run_liveness`]. The third state ([`RunLiveness::Undetermined`]) is what
/// keeps the merge-hold gate from silently failing open: it lets the caller
/// tell "positively dead" apart from "could not find out", and fail CLOSED on
/// the latter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunLiveness {
    /// `run_id` holds ≥1 claim whose heartbeat is within the stuck-TTL.
    Live,
    /// The registry was read cleanly and holds no fresh claim for `run_id`
    /// (a genuinely-absent registry = no claims = also Dead).
    Dead,
    /// Liveness could not be established (unreadable/corrupt registry, or an
    /// empty/unattributed `run_id`). Callers that gate on death MUST treat this
    /// as "still live" (fail closed).
    Undetermined,
}

/// Liveness of `run_id` in this project's claim registry, for the runtime-overlap
/// merge-hold gate ([`crate::worktree`]).
///
/// This is condukt's OWN liveness signal, in condukt's OWN run-id space: a live
/// run refreshes the heartbeat of the claims it holds (`state set --status
/// running` claims a task's files under the run id; every later `state set` /
/// `state heartbeat` refreshes them via [`heartbeat`]), so a claim with a fresh
/// heartbeat proves its run is alive. A run that has since crashed/been
/// abandoned stops heart-beating and its claims age out past the TTL
/// ([`is_stale`]) — exactly the same staleness the reaper uses. The hold's
/// `run_id` IS a condukt run id, so it shares this registry's identity space —
/// the whole point of keying liveness here rather than off a disjoint id space.
///
/// The gate uses this to decide whether a hold is still authoritative: a hold
/// placed by a LIVE run keeps blocking the merge, while a hold left by a DEAD
/// run ages out so it does not block a live task that merely REUSES the same
/// `condukt/<task.id>` branch name.
///
/// **Fail-closed contract.** Only a CLEAN registry read that finds no fresh
/// claim returns [`RunLiveness::Dead`]. A present-but-unreadable/corrupt
/// registry, or an empty `run_id` (an unattributed hold with no placer to test),
/// returns [`RunLiveness::Undetermined`] — never `Dead`. Collapsing "cannot read
/// the registry" into "the placer is dead, drop the hold" would re-enter the
/// reverted disjoint-id bug's failure class (an empty read making every hold
/// look dead → the gate silently off) one layer down; the caller must fail
/// closed on `Undetermined`.
///
/// A pure, lock-free read: it never persists (no reap-compaction) and never
/// takes the claims lock, so a merge liveness probe cannot contend with a live
/// run's claim writes. The atomic-rename saves mean this always observes a
/// COMPLETE registry (never a half-written one), so a torn read cannot
/// masquerade as a clean empty registry.
pub fn run_liveness(cfg: &Config, cwd: &Path, run_id: &str, now: i64) -> RunLiveness {
    if run_id.is_empty() {
        // No placer id to test — death cannot be established. Fail closed.
        return RunLiveness::Undetermined;
    }
    let reg = match read_registry(&registry_path(cfg, cwd)) {
        Ok(Some(reg)) => reg,
        // Genuinely-absent registry: no claims exist for anyone ⇒ Dead.
        Ok(None) => return RunLiveness::Dead,
        // Present but unreadable/corrupt: liveness UNKNOWN. Fail closed.
        Err(_) => return RunLiveness::Undetermined,
    };
    let ttl = ttl_secs(cfg);
    let live = reg
        .files
        .values()
        .chain(reg.task_claims.values())
        .any(|c| c.run_id == run_id && !is_stale(c, now, ttl));
    if live {
        RunLiveness::Live
    } else {
        RunLiveness::Dead
    }
}

/// Read the claim registry distinguishing a genuinely-absent file (`Ok(None)`)
/// from a present-but-unreadable/corrupt one (`Err`), so a caller that must fail
/// CLOSED on "cannot determine" can tell the two apart. Plain [`load`]
/// deliberately collapses both to an empty registry for callers that are fine
/// degrading to no-claims; [`run_liveness`] cannot use it because that collapse
/// is exactly the silent fail-open it must avoid.
fn read_registry(path: &Path) -> std::io::Result<Option<Registry>> {
    match std::fs::read_to_string(path) {
        Ok(txt) => serde_json::from_str(&txt)
            .map(Some)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Test-only accessor for the private registry path, so cross-module tests
/// (e.g. the worktree merge-hold gate) can seed a CORRUPT registry at exactly
/// the location [`run_liveness`] reads.
#[cfg(test)]
pub(crate) fn registry_path_for_test(cfg: &Config, cwd: &Path) -> PathBuf {
    registry_path(cfg, cwd)
}

/// Who holds a task claim — the observability slice of a [`Claim`] for the
/// execution-state view.
#[allow(dead_code)] // consumed by the CLI wiring in the follow-up task
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaimedBy {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub run_id: String,
    pub heartbeat_at: i64,
}

/// One row of the execution-state view: a live task claim joined against the
/// project backlog. `backlog_id`/`status`/`project` are `None` when the backlog is
/// unavailable or the claim's title matches no pending task (fail-soft join).
#[allow(dead_code)] // consumed by the CLI wiring in the follow-up task
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionEntry {
    pub hashkey: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backlog_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    pub claimed_by: ClaimedBy,
}

/// Coerce a backlog JSON `id` (string or number) into a display string.
#[allow(dead_code)]
fn json_id(v: Option<&serde_json::Value>) -> Option<String> {
    match v {
        Some(serde_json::Value::String(s)) => Some(s.clone()),
        Some(serde_json::Value::Number(n)) => Some(n.to_string()),
        _ => None,
    }
}

/// Shell out to `backlog list --status pending --json` and return the pending
/// tasks as a list of JSON objects. Fail-soft: a missing binary, non-zero exit, or
/// unparseable output yields an empty list rather than an error (never panics).
/// Accepts either a top-level array or a `{ "tasks": [..] }` envelope.
#[allow(dead_code)]
fn backlog_pending() -> Vec<serde_json::Value> {
    let output = std::process::Command::new("backlog")
        .args(["list", "--status", "pending", "--json"])
        .output();
    let stdout = match output {
        Ok(o) if o.status.success() => o.stdout,
        _ => return Vec::new(),
    };
    match serde_json::from_slice::<serde_json::Value>(&stdout) {
        Ok(serde_json::Value::Array(a)) => a,
        Ok(serde_json::Value::Object(mut m)) => m
            .remove("tasks")
            .and_then(|t| match t {
                serde_json::Value::Array(a) => Some(a),
                _ => None,
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// Pure JOIN of live task claims against backlog pending tasks, keyed on title
/// (the one field a claim and a backlog row share here — hashkeys are opaque to
/// the backlog). Split out from [`write_execution_state`] so it is testable without
/// the external `backlog` binary. Deterministic ordering (task_claims is a BTreeMap).
#[allow(dead_code)]
fn join_execution_state(
    task_claims: &BTreeMap<String, Claim>,
    pending: &[serde_json::Value],
) -> Vec<ExecutionEntry> {
    // Index pending tasks by title for the join.
    let mut by_title: BTreeMap<&str, &serde_json::Value> = BTreeMap::new();
    for t in pending {
        if let Some(title) = t.get("title").and_then(|v| v.as_str()) {
            by_title.insert(title, t);
        }
    }
    task_claims
        .iter()
        .map(|(hashkey, claim)| {
            let matched = claim
                .title
                .as_deref()
                .and_then(|t| by_title.get(t).copied());
            ExecutionEntry {
                hashkey: hashkey.clone(),
                backlog_id: matched.and_then(|t| json_id(t.get("id"))),
                title: claim.title.clone(),
                project: matched
                    .and_then(|t| t.get("project"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                status: matched
                    .and_then(|t| t.get("status"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                claimed_by: ClaimedBy {
                    session_id: claim.session_id.clone(),
                    run_id: claim.run_id.clone(),
                    heartbeat_at: claim.heartbeat_at,
                },
            }
        })
        .collect()
}

/// Aggregate the live task claims into `execution-state.json` (beside
/// `claims.json`): reap stale claims, JOIN the survivors against the backlog's
/// pending tasks, and atomically write the rows. Fail-soft on the backlog (see
/// [`backlog_pending`]) — the claims we hold are always written, with
/// `backlog_id`/`status`/`project` left absent when unjoinable. Returns the rows
/// written.
// Wired into the CLI by the follow-up task.
#[allow(dead_code)]
pub fn write_execution_state(cfg: &Config, cwd: &Path, now: i64) -> Result<Vec<ExecutionEntry>> {
    let ttl = ttl_secs(cfg);
    let path = registry_path(cfg, cwd);
    // On contention, degrade to a read-only view: compute and return the joined
    // rows from a freshly-loaded, reaped snapshot but persist NOTHING (neither
    // the registry compaction nor the execution-state file), rather than run an
    // unlocked RMW that could clobber a concurrent writer. The view regenerates
    // on the next uncontended call.
    match RunLock::acquire_or_skip(cfg, cwd, CLAIMS_LOCK_KEY) {
        Some(_lock) => {
            let mut reg = load(&path);
            reap(&mut reg, now, ttl, &|c| claim_progress(cfg, cwd, c, now));
            save(&path, &reg)?;
            let pending = backlog_pending();
            let entries = join_execution_state(&reg.task_claims, &pending);
            save(&execution_state_path(cfg, cwd), &entries)?;
            Ok(entries)
        }
        None => {
            let mut reg = load(&path);
            reap(&mut reg, now, ttl, &|c| claim_progress(cfg, cwd, c, now));
            let pending = backlog_pending();
            Ok(join_execution_state(&reg.task_claims, &pending))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQ: AtomicU64 = AtomicU64::new(0);

    fn make_tmp_dir(tag: &str) -> PathBuf {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let p =
            std::env::temp_dir().join(format!("condukt-claim-{tag}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    // `unique_tmp` must derive a DISTINCT temp path per call even within one
    // process (same pid, and possibly an identical coarse-clock nanos). The old
    // fixed `path.with_extension("json.tmp")` yielded ONE shared name for every
    // writer — the collision that lets two degraded writers publish a
    // half-written registry. RED with the fixed name (all equal), GREEN here.
    #[test]
    fn unique_tmp_names_are_distinct_per_call() {
        let path = make_tmp_dir("uniq").join("claims.json");
        let a = unique_tmp(&path, "json");
        let b = unique_tmp(&path, "json");
        let c = unique_tmp(&path, "json");
        assert_ne!(a, b);
        assert_ne!(b, c);
        assert_ne!(a, c);
        // Still a sibling temp of the final path (same parent, distinct name).
        assert_eq!(a.parent(), path.parent());
        assert_ne!(a.file_name(), path.file_name());
    }

    // Two concurrent degraded writers saving the SAME registry to one path must
    // never leave a half-written/empty file behind: with unique temp names each
    // writer renames its OWN fully-written temp atomically, so a concurrent
    // reader always sees a complete, parseable registry. Under the old fixed
    // `json.tmp` name both writers share one temp and one can rename a partially
    // written file (corrupt → loads empty → mass double-claim). Large payload so
    // a single `write` is not atomic at the OS level (widens the corrupt window).
    #[test]
    fn concurrent_saves_never_publish_corrupt_registry() {
        let path = make_tmp_dir("concurrent-save").join("claims.json");
        // A non-trivial registry so a truncated/interleaved write fails to parse.
        let mut reg = Registry::default();
        for i in 0..400 {
            reg.files.insert(
                format!("crates/pkg/src/file_{i:04}.rs"),
                Claim {
                    run_id: format!("run-{i:04}"),
                    pid: 12345,
                    session_id: Some(format!("session-{i:04}")),
                    heartbeat_at: 1_700_000_000 + i as i64,
                    claimed_at: 1_700_000_000,
                    title: Some(format!("task number {i} with a longish title")),
                },
            );
        }

        const THREADS: usize = 8;
        const ITERS: usize = 40;
        std::thread::scope(|scope| {
            for _ in 0..THREADS {
                let path = path.clone();
                let reg = reg.clone();
                scope.spawn(move || {
                    for _ in 0..ITERS {
                        save(&path, &reg).unwrap();
                        // Interleave reads: every observed file must be a
                        // complete, parseable registry (never half-written).
                        let loaded = load(&path);
                        assert_eq!(
                            loaded.files.len(),
                            reg.files.len(),
                            "a concurrent reader observed a corrupt/partial registry"
                        );
                    }
                });
            }
        });

        // Final state is intact.
        let final_reg = load(&path);
        assert_eq!(final_reg.files.len(), reg.files.len());
    }

    /// A fresh claim ⇒ `Live`. Establishes liveness via the real claim path.
    #[test]
    fn run_liveness_live_for_fresh_claim() {
        let tmp = make_tmp_dir("live");
        let cfg = make_cfg(&tmp);
        let now = 1_000_000;
        claim_files(&cfg, &tmp, "run", Some("sess"), &files(&["a.txt"]), now).unwrap();
        assert_eq!(run_liveness(&cfg, &tmp, "run", now), RunLiveness::Live);
    }

    /// A genuinely-absent registry ⇒ `Dead` (no claims exist for anyone). This
    /// is the normal "the placer released/never wrote claims" case, and it must
    /// stay `Dead` so a stale hold from a finished run does not block forever.
    #[test]
    fn run_liveness_dead_when_registry_absent() {
        let tmp = make_tmp_dir("absent");
        let cfg = make_cfg(&tmp);
        // No claim ever written → claims.json does not exist.
        assert!(!registry_path(&cfg, &tmp).exists());
        assert_eq!(
            run_liveness(&cfg, &tmp, "run", 1_000_000),
            RunLiveness::Dead
        );
    }

    /// A claim whose heartbeat is past the stuck-TTL ⇒ `Dead` (the placer went
    /// silent — the reaper's own staleness).
    #[test]
    fn run_liveness_dead_when_claim_stale() {
        let tmp = make_tmp_dir("stale");
        let cfg = make_cfg(&tmp);
        let claimed_at = 1_000_000;
        claim_files(
            &cfg,
            &tmp,
            "run",
            Some("sess"),
            &files(&["a.txt"]),
            claimed_at,
        )
        .unwrap();
        let now = claimed_at + cfg.stuck_ttl_secs as i64 + 100;
        assert_eq!(run_liveness(&cfg, &tmp, "run", now), RunLiveness::Dead);
    }

    /// A present-but-UNREADABLE registry ⇒ `Undetermined`, NOT `Dead`. This is
    /// the fail-closed pin: a corrupt claims.json must never be silently read as
    /// "the placer is dead", which would let the merge-hold gate drop a hold it
    /// cannot vouch for. RED against the old `load()`-to-empty behavior (which
    /// returned "not live" ⇒ the gate would map it to Dead and merge).
    #[test]
    fn run_liveness_undetermined_when_registry_unreadable() {
        let tmp = make_tmp_dir("corrupt");
        let cfg = make_cfg(&tmp);
        let path = registry_path(&cfg, &tmp);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"{ this is not valid json ]]").unwrap();
        assert_eq!(
            run_liveness(&cfg, &tmp, "run", 1_000_000),
            RunLiveness::Undetermined
        );
    }

    /// An empty `run_id` (an unattributed hold, no placer to test) ⇒
    /// `Undetermined`, so the gate keeps blocking rather than dropping a hold on
    /// ambiguous data.
    #[test]
    fn run_liveness_undetermined_for_empty_run_id() {
        let tmp = make_tmp_dir("empty-id");
        let cfg = make_cfg(&tmp);
        assert_eq!(
            run_liveness(&cfg, &tmp, "", 1_000_000),
            RunLiveness::Undetermined
        );
    }

    fn make_cfg(tmp: &Path) -> Config {
        Config {
            worktree_base: tmp.join("worktrees"),
            default_branch: "main".to_string(),
            shared_globs: Vec::new(),
            max_parallel: 4,
            state_dir: tmp.to_path_buf(),
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

    fn files(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn first_claim_succeeds_and_persists() {
        let tmp = make_tmp_dir("first");
        let cfg = make_cfg(&tmp);
        let out = claim_files(
            &cfg,
            &tmp,
            "runA",
            Some("sessA"),
            &files(&["src/a.rs"]),
            100,
        )
        .unwrap();
        assert_eq!(out.claimed, vec!["src/a.rs".to_string()]);
        assert!(out.skipped.is_empty());
        // Persisted and visible.
        let live = active_claims(&cfg, &tmp, 100).unwrap();
        assert_eq!(live.files.len(), 1);
        assert_eq!(live.files["src/a.rs"].run_id, "runA");
    }

    #[test]
    fn conflicting_claim_from_other_run_is_hard_skipped() {
        let tmp = make_tmp_dir("conflict");
        let cfg = make_cfg(&tmp);
        claim_files(&cfg, &tmp, "runA", None, &files(&["src/a.rs"]), 100).unwrap();
        // Different run tries the same file → skipped, not claimed.
        let out = claim_files(&cfg, &tmp, "runB", None, &files(&["src/a.rs"]), 101).unwrap();
        assert!(out.claimed.is_empty());
        assert_eq!(out.skipped.len(), 1);
        assert_eq!(out.skipped[0].holder_run, "runA");
        // The file is still owned by runA.
        let live = active_claims(&cfg, &tmp, 102).unwrap();
        assert_eq!(live.files["src/a.rs"].run_id, "runA");
    }

    #[test]
    fn partial_batch_claims_free_files_only() {
        let tmp = make_tmp_dir("partial");
        let cfg = make_cfg(&tmp);
        claim_files(&cfg, &tmp, "runA", None, &files(&["src/a.rs"]), 100).unwrap();
        let out = claim_files(
            &cfg,
            &tmp,
            "runB",
            None,
            &files(&["src/a.rs", "src/b.rs"]),
            101,
        )
        .unwrap();
        assert_eq!(out.claimed, vec!["src/b.rs".to_string()]);
        assert_eq!(out.skipped.len(), 1);
        assert_eq!(out.skipped[0].file, "src/a.rs");
    }

    #[test]
    fn glob_overlap_is_detected() {
        let tmp = make_tmp_dir("glob");
        let cfg = make_cfg(&tmp);
        claim_files(&cfg, &tmp, "runA", None, &files(&["src/*.rs"]), 100).unwrap();
        let out = claim_files(&cfg, &tmp, "runB", None, &files(&["src/a.rs"]), 101).unwrap();
        assert!(out.claimed.is_empty(), "src/a.rs overlaps src/*.rs");
        assert_eq!(out.skipped.len(), 1);
    }

    #[test]
    fn same_run_reclaim_refreshes_heartbeat_and_keeps_claimed_at() {
        let tmp = make_tmp_dir("reclaim");
        let cfg = make_cfg(&tmp);
        claim_files(&cfg, &tmp, "runA", None, &files(&["src/a.rs"]), 100).unwrap();
        let out = claim_files(&cfg, &tmp, "runA", None, &files(&["src/a.rs"]), 200).unwrap();
        assert_eq!(out.claimed, vec!["src/a.rs".to_string()]);
        let live = active_claims(&cfg, &tmp, 200).unwrap();
        assert_eq!(live.files["src/a.rs"].claimed_at, 100);
        assert_eq!(live.files["src/a.rs"].heartbeat_at, 200);
    }

    #[test]
    fn release_files_frees_the_slot() {
        let tmp = make_tmp_dir("release");
        let cfg = make_cfg(&tmp);
        claim_files(&cfg, &tmp, "runA", None, &files(&["src/a.rs"]), 100).unwrap();
        let n = release_files(&cfg, &tmp, "runA", &files(&["src/a.rs"])).unwrap();
        assert_eq!(n, 1);
        // Now runB can take it.
        let out = claim_files(&cfg, &tmp, "runB", None, &files(&["src/a.rs"]), 101).unwrap();
        assert_eq!(out.claimed, vec!["src/a.rs".to_string()]);
    }

    #[test]
    fn release_run_frees_all_its_files() {
        let tmp = make_tmp_dir("release-run");
        let cfg = make_cfg(&tmp);
        claim_files(
            &cfg,
            &tmp,
            "runA",
            None,
            &files(&["src/a.rs", "src/b.rs"]),
            100,
        )
        .unwrap();
        let n = release_run(&cfg, &tmp, "runA").unwrap();
        assert_eq!(n, 2);
        assert!(active_claims(&cfg, &tmp, 101).unwrap().files.is_empty());
    }

    #[test]
    fn stale_claim_with_confirmed_stalled_progress_is_reaped_and_reclaimable() {
        let tmp = make_tmp_dir("stale-hb");
        let cfg = make_cfg(&tmp);
        // A claim whose heartbeat is far in the past — the holding session went
        // quiet without releasing. Heartbeat-staleness alone is NOT enough to
        // reap now; the run's progress must be CONFIRMED Stalled. Force that
        // verdict via the thread-local test seam (production samples git HEAD +
        // transcript + task updated_at across the window instead).
        let mut reg = Registry::default();
        reg.files.insert(
            "src/a.rs".to_string(),
            Claim {
                run_id: "ghost".into(),
                session_id: Some("dead-session".into()),
                pid: std::process::id(),
                claimed_at: 0,
                heartbeat_at: 0,
                title: None,
            },
        );
        save(&registry_path(&cfg, &tmp), &reg).unwrap();
        // now = ttl + 1 past the heartbeat → eligible; confirmed Stalled → reaped.
        let now = cfg.stuck_ttl_secs as i64 + 1;
        let out = test_hook::with_forced(Determination::Known(Liveness::Stalled), || {
            claim_files(&cfg, &tmp, "runB", None, &files(&["src/a.rs"]), now).unwrap()
        });
        assert_eq!(out.claimed, vec!["src/a.rs".to_string()]);
        assert!(out.skipped.is_empty());
    }

    #[test]
    fn stale_claim_but_progressing_is_not_reaped() {
        let tmp = make_tmp_dir("stale-hb-progressing");
        let cfg = make_cfg(&tmp);
        // Same heartbeat-stale claim, but the run is still doing durable work.
        // The quiet holder must be PRESERVED — reaping it would force-steal a
        // live-but-quiet session (the exact fail-open this gate closes).
        let mut reg = Registry::default();
        reg.files.insert(
            "src/a.rs".to_string(),
            Claim {
                run_id: "busy".into(),
                session_id: Some("live-quiet".into()),
                pid: std::process::id(),
                claimed_at: 0,
                heartbeat_at: 0,
                title: None,
            },
        );
        save(&registry_path(&cfg, &tmp), &reg).unwrap();
        let now = cfg.stuck_ttl_secs as i64 + 1;
        let out = test_hook::with_forced(Determination::Known(Liveness::Progressing), || {
            claim_files(&cfg, &tmp, "runB", None, &files(&["src/a.rs"]), now).unwrap()
        });
        assert!(out.claimed.is_empty(), "progressing holder must be kept");
        assert_eq!(out.skipped.len(), 1);
        assert_eq!(out.skipped[0].holder_run, "busy");
    }

    #[test]
    fn stale_claim_with_undetermined_progress_is_not_reaped() {
        let tmp = make_tmp_dir("stale-hb-undet");
        let cfg = make_cfg(&tmp);
        // Progress could not be established (unreadable signals / no prior sample
        // / window not elapsed). "Cannot determine" must resolve to the
        // restrictive side: the claim is KEPT, never reaped.
        let mut reg = Registry::default();
        reg.files.insert(
            "src/a.rs".to_string(),
            Claim {
                run_id: "unknown".into(),
                session_id: Some("opaque".into()),
                pid: std::process::id(),
                claimed_at: 0,
                heartbeat_at: 0,
                title: None,
            },
        );
        save(&registry_path(&cfg, &tmp), &reg).unwrap();
        let now = cfg.stuck_ttl_secs as i64 + 1;
        let undet = Determination::undetermined("no sample yet");
        let out = test_hook::with_forced(undet, || {
            claim_files(&cfg, &tmp, "runB", None, &files(&["src/a.rs"]), now).unwrap()
        });
        assert!(
            out.claimed.is_empty(),
            "undetermined progress must NOT reap"
        );
        assert_eq!(out.skipped.len(), 1);
        assert_eq!(out.skipped[0].holder_run, "unknown");
    }

    #[test]
    fn fresh_heartbeat_is_not_reaped_regardless_of_pid() {
        let tmp = make_tmp_dir("fresh-hb");
        let cfg = make_cfg(&tmp);
        // Hand-write a claim from a pid that is certainly not this process, but
        // with a fresh heartbeat: it must be RETAINED (pid is not a liveness
        // signal — the ephemeral-CLI reality).
        let mut reg = Registry::default();
        reg.files.insert(
            "src/a.rs".to_string(),
            Claim {
                run_id: "other".into(),
                session_id: Some("live-elsewhere".into()),
                pid: 424242,
                claimed_at: 100,
                heartbeat_at: 100,
                title: None,
            },
        );
        save(&registry_path(&cfg, &tmp), &reg).unwrap();
        // Within TTL of the heartbeat → still held → runB is skipped.
        let out = claim_files(&cfg, &tmp, "runB", None, &files(&["src/a.rs"]), 200).unwrap();
        assert!(out.claimed.is_empty());
        assert_eq!(out.skipped.len(), 1);
        assert_eq!(out.skipped[0].holder_run, "other");
    }

    #[test]
    fn heartbeat_keeps_live_claim_protected() {
        let tmp = make_tmp_dir("hb-protect");
        let cfg = make_cfg(&tmp);
        claim_files(&cfg, &tmp, "runA", None, &files(&["src/a.rs"]), 0).unwrap();
        // Heartbeat just before the TTL window would expire.
        let ttl = cfg.stuck_ttl_secs as i64;
        heartbeat(&cfg, &tmp, "runA", ttl).unwrap();
        // A later claim from runB, within TTL of the heartbeat, is still skipped.
        let out = claim_files(&cfg, &tmp, "runB", None, &files(&["src/a.rs"]), ttl + 1).unwrap();
        assert!(out.claimed.is_empty());
        assert_eq!(out.skipped.len(), 1);
    }

    #[test]
    fn corrupt_registry_is_treated_as_empty() {
        let tmp = make_tmp_dir("corrupt");
        let cfg = make_cfg(&tmp);
        let path = registry_path(&cfg, &tmp);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"not json at all {{{").unwrap();
        // Fail-soft: claim still succeeds (registry read as empty).
        let out = claim_files(&cfg, &tmp, "runA", None, &files(&["src/a.rs"]), 100).unwrap();
        assert_eq!(out.claimed, vec!["src/a.rs".to_string()]);
    }

    // ---- task-level claims ------------------------------------------------

    #[test]
    fn task_claim_succeeds_and_persists_with_title() {
        let tmp = make_tmp_dir("task-first");
        let cfg = make_cfg(&tmp);
        let out = claim_tasks(
            &cfg,
            &tmp,
            &files(&["hk-1"]),
            "runA",
            Some("sessA"),
            100,
            Some("do the thing"),
        )
        .unwrap();
        assert_eq!(out.claimed, vec!["hk-1".to_string()]);
        assert!(out.skipped.is_empty());
        let live = active_claims(&cfg, &tmp, 100).unwrap();
        assert_eq!(live.task_claims["hk-1"].run_id, "runA");
        assert_eq!(
            live.task_claims["hk-1"].title.as_deref(),
            Some("do the thing")
        );
        // Task claims live in their own table, not the file table.
        assert!(live.files.is_empty());
    }

    #[test]
    fn same_task_held_by_another_live_run_is_skipped() {
        let tmp = make_tmp_dir("task-conflict");
        let cfg = make_cfg(&tmp);
        claim_tasks(&cfg, &tmp, &files(&["hk-1"]), "runA", None, 100, None).unwrap();
        let out = claim_tasks(&cfg, &tmp, &files(&["hk-1"]), "runB", None, 101, None).unwrap();
        assert!(out.claimed.is_empty());
        assert_eq!(out.skipped.len(), 1);
        assert_eq!(out.skipped[0].file, "hk-1");
        assert_eq!(out.skipped[0].holder_run, "runA");
        // Still owned by runA.
        let live = active_claims(&cfg, &tmp, 102).unwrap();
        assert_eq!(live.task_claims["hk-1"].run_id, "runA");
    }

    #[test]
    fn stale_task_claim_with_confirmed_stalled_progress_is_reaped_and_reclaimable() {
        let tmp = make_tmp_dir("task-stale");
        let cfg = make_cfg(&tmp);
        let mut reg = Registry::default();
        reg.task_claims.insert(
            "hk-1".to_string(),
            Claim {
                run_id: "ghost".into(),
                session_id: Some("dead".into()),
                pid: std::process::id(),
                claimed_at: 0,
                heartbeat_at: 0,
                title: Some("stale task".into()),
            },
        );
        save(&registry_path(&cfg, &tmp), &reg).unwrap();
        // now past the TTL of the heartbeat → eligible; confirmed Stalled → reaped.
        let now = cfg.stuck_ttl_secs as i64 + 1;
        let out = test_hook::with_forced(Determination::Known(Liveness::Stalled), || {
            // The reclaiming acquire AND the observability read both reap under a
            // Stalled verdict, so hold the seam across both.
            let out = claim_tasks(&cfg, &tmp, &files(&["hk-1"]), "runB", None, now, None).unwrap();
            let live = active_claims(&cfg, &tmp, now).unwrap();
            assert_eq!(live.task_claims["hk-1"].run_id, "runB");
            out
        });
        assert_eq!(out.claimed, vec!["hk-1".to_string()]);
        assert!(out.skipped.is_empty());
    }

    #[test]
    fn stale_task_claim_but_progressing_is_not_reaped() {
        let tmp = make_tmp_dir("task-stale-progressing");
        let cfg = make_cfg(&tmp);
        let mut reg = Registry::default();
        reg.task_claims.insert(
            "hk-1".to_string(),
            Claim {
                run_id: "busy".into(),
                session_id: Some("live".into()),
                pid: std::process::id(),
                claimed_at: 0,
                heartbeat_at: 0,
                title: Some("still working".into()),
            },
        );
        save(&registry_path(&cfg, &tmp), &reg).unwrap();
        let now = cfg.stuck_ttl_secs as i64 + 1;
        let out = test_hook::with_forced(Determination::Known(Liveness::Progressing), || {
            claim_tasks(&cfg, &tmp, &files(&["hk-1"]), "runB", None, now, None).unwrap()
        });
        assert!(
            out.claimed.is_empty(),
            "progressing task holder must be kept"
        );
        assert_eq!(out.skipped.len(), 1);
        assert_eq!(out.skipped[0].holder_run, "busy");
    }

    #[test]
    fn release_tasks_frees_the_hashkey() {
        let tmp = make_tmp_dir("task-release");
        let cfg = make_cfg(&tmp);
        claim_tasks(&cfg, &tmp, &files(&["hk-1"]), "runA", None, 100, None).unwrap();
        let n = release_tasks(&cfg, &tmp, &files(&["hk-1"])).unwrap();
        assert_eq!(n, 1);
        // Now runB can take it.
        let out = claim_tasks(&cfg, &tmp, &files(&["hk-1"]), "runB", None, 101, None).unwrap();
        assert_eq!(out.claimed, vec!["hk-1".to_string()]);
    }

    #[test]
    fn release_run_drops_both_file_and_task_claims() {
        let tmp = make_tmp_dir("task-release-run");
        let cfg = make_cfg(&tmp);
        claim_files(&cfg, &tmp, "runA", None, &files(&["src/a.rs"]), 100).unwrap();
        claim_tasks(&cfg, &tmp, &files(&["hk-1"]), "runA", None, 100, None).unwrap();
        let n = release_run(&cfg, &tmp, "runA").unwrap();
        assert_eq!(n, 2, "one file + one task claim released");
        let live = active_claims(&cfg, &tmp, 101).unwrap();
        assert!(live.files.is_empty());
        assert!(live.task_claims.is_empty());
    }

    #[test]
    fn heartbeat_refreshes_both_file_and_task_claims() {
        let tmp = make_tmp_dir("task-hb");
        let cfg = make_cfg(&tmp);
        claim_files(&cfg, &tmp, "runA", None, &files(&["src/a.rs"]), 0).unwrap();
        claim_tasks(&cfg, &tmp, &files(&["hk-1"]), "runA", None, 0, None).unwrap();
        let n = heartbeat(&cfg, &tmp, "runA", 500).unwrap();
        assert_eq!(n, 2, "both the file and the task claim refreshed");
        let live = active_claims(&cfg, &tmp, 500).unwrap();
        assert_eq!(live.files["src/a.rs"].heartbeat_at, 500);
        assert_eq!(live.task_claims["hk-1"].heartbeat_at, 500);
    }

    // ---- execution-state view --------------------------------------------

    #[test]
    fn execution_join_emits_status_and_claimed_by() {
        // Pure join: a claim whose title matches a pending backlog row gets enriched.
        let mut task_claims = BTreeMap::new();
        task_claims.insert(
            "hk-1".to_string(),
            Claim {
                run_id: "runA".into(),
                session_id: Some("sessA".into()),
                pid: 1,
                claimed_at: 10,
                heartbeat_at: 42,
                title: Some("wire the CLI".into()),
            },
        );
        // An unmatched claim (no backlog row) must still appear, with null backlog.
        task_claims.insert(
            "hk-2".to_string(),
            Claim {
                run_id: "runB".into(),
                session_id: None,
                pid: 2,
                claimed_at: 10,
                heartbeat_at: 43,
                title: Some("orphan task".into()),
            },
        );
        let pending = vec![serde_json::json!({
            "id": 77,
            "title": "wire the CLI",
            "status": "pending",
            "project": "condukt",
        })];
        let rows = join_execution_state(&task_claims, &pending);
        assert_eq!(rows.len(), 2);
        // BTreeMap order → hk-1 first.
        let r0 = &rows[0];
        assert_eq!(r0.hashkey, "hk-1");
        assert_eq!(r0.backlog_id.as_deref(), Some("77"));
        assert_eq!(r0.status.as_deref(), Some("pending"));
        assert_eq!(r0.project.as_deref(), Some("condukt"));
        assert_eq!(r0.title.as_deref(), Some("wire the CLI"));
        assert_eq!(r0.claimed_by.run_id, "runA");
        assert_eq!(r0.claimed_by.session_id.as_deref(), Some("sessA"));
        assert_eq!(r0.claimed_by.heartbeat_at, 42);
        // Unmatched row: claim data present, backlog fields absent.
        let r1 = &rows[1];
        assert_eq!(r1.hashkey, "hk-2");
        assert!(r1.backlog_id.is_none());
        assert!(r1.status.is_none());
        assert_eq!(r1.claimed_by.run_id, "runB");
    }

    #[test]
    fn write_execution_state_is_fail_soft_without_backlog() {
        // With no `backlog` binary on PATH the join yields null backlog fields, but
        // the held claims are still written and returned.
        let tmp = make_tmp_dir("exec-failsoft");
        let cfg = make_cfg(&tmp);
        claim_tasks(
            &cfg,
            &tmp,
            &files(&["hk-1"]),
            "runA",
            Some("sessA"),
            100,
            Some("a task"),
        )
        .unwrap();
        let rows = write_execution_state(&cfg, &tmp, 100).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].hashkey, "hk-1");
        assert_eq!(rows[0].claimed_by.run_id, "runA");
        assert_eq!(rows[0].title.as_deref(), Some("a task"));
        // The file was written beside claims.json and round-trips.
        let path = execution_state_path(&cfg, &tmp);
        let txt = std::fs::read_to_string(&path).unwrap();
        let parsed: Vec<ExecutionEntry> = serde_json::from_str(&txt).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].hashkey, "hk-1");
    }

    // A wedged holder of the claims-registry lock must make the REAL production
    // claim path HARD-SKIP (report every file skipped, claim none, persist
    // nothing) rather than degrade to an unlocked read-modify-write that could
    // double-claim. RED with the fail-soft `RunLock::acquire` this crate used to
    // expose (since REMOVED): it handed back an unlocked guard, the RMW
    // proceeded, and the files were claimed (claimed non-empty, registry
    // written). GREEN with `acquire_or_skip_with_deadline`.
    #[test]
    fn contended_claims_lock_makes_claim_files_hard_skip_not_double_claim() {
        use std::time::Duration;
        let tmp = make_tmp_dir("wedge-skip");
        let cfg = make_cfg(&tmp);

        // Wedge a live holder of the reserved claims lock for the whole test.
        let held = RunLock::acquire_or_skip(&cfg, &tmp, CLAIMS_LOCK_KEY)
            .expect("precondition: wedge must genuinely hold the claims lock");
        assert!(
            held.held(),
            "precondition: wedge must genuinely hold the claims lock"
        );

        // The real production path (claim_files -> claim_files_with_deadline)
        // must skip every file under a short deadline rather than proceed
        // unlocked and double-claim.
        let out = claim_files_with_deadline(
            &cfg,
            &tmp,
            "runB",
            Some("sessB"),
            &files(&["src/a.rs", "src/b.rs"]),
            100,
            Duration::from_millis(120),
        )
        .unwrap();

        assert!(
            out.claimed.is_empty(),
            "contended claim must claim NOTHING, got: {:?}",
            out.claimed
        );
        assert_eq!(
            out.skipped.len(),
            2,
            "contended claim must report every requested file skipped"
        );
        assert!(
            out.skipped
                .iter()
                .all(|s| s.holder_run == LOCK_CONTENDED_HOLDER),
            "skips must be marked as lock-contention skips"
        );

        // Nothing was written to the registry (no unlocked double-claim).
        let path = registry_path(&cfg, &tmp);
        assert!(
            !path.exists() || load(&path).files.is_empty(),
            "a contended claim must not persist any claim to the registry"
        );

        drop(held);
    }

    #[test]
    fn old_bare_map_claims_json_still_loads() {
        // Backward compat: a pre-task-claims registry was a bare file->claim map.
        // The flattened `files` field must absorb it.
        let tmp = make_tmp_dir("backcompat");
        let cfg = make_cfg(&tmp);
        let path = registry_path(&cfg, &tmp);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            br#"{"src/a.rs":{"run_id":"runA","pid":1,"claimed_at":5,"heartbeat_at":5}}"#,
        )
        .unwrap();
        let live = active_claims(&cfg, &tmp, 6).unwrap();
        assert_eq!(live.files["src/a.rs"].run_id, "runA");
        assert!(live.task_claims.is_empty());
    }
}
