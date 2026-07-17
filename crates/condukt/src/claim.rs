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
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Reserved run-id used only to key the registry's own RMW lock file
/// (`<project>/__claims__.lock`), reusing the proven per-run lock. It never names
/// a real run, so it cannot collide with one.
const CLAIMS_LOCK_KEY: &str = "__claims__";

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

/// A claim is stale when its heartbeat is older than the TTL — the session that
/// held it died (or stalled past the stuck-TTL) without releasing.
///
/// Liveness is anchored to the **heartbeat**, NOT to `c.pid`: the condukt CLI is
/// ephemeral (each invocation is a separate short-lived process that exits
/// immediately), so the recorded pid is essentially always dead by the next
/// command. Reaping on pid-liveness would therefore wipe every claim on the very
/// next invocation. A live session instead proves liveness by refreshing the
/// heartbeat (via `state set`/`state heartbeat`); `c.pid` is retained only for
/// observability (which condukt process last touched the claim). The TTL reuses
/// the STUCK TTL, so a claim becomes reclaimable exactly when its task would
/// already be considered stuck.
fn is_stale(c: &Claim, now: i64, ttl: i64) -> bool {
    now.saturating_sub(c.heartbeat_at) > ttl
}

fn reap(reg: &mut Registry, now: i64, ttl: i64) {
    reg.files.retain(|_, c| !is_stale(c, now, ttl));
    reg.task_claims.retain(|_, c| !is_stale(c, now, ttl));
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
    let ttl = ttl_secs(cfg);
    let path = registry_path(cfg, cwd);
    let _lock = RunLock::acquire(cfg, cwd, CLAIMS_LOCK_KEY);
    let mut reg = load(&path);
    reap(&mut reg, now, ttl);

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
    let _lock = RunLock::acquire(cfg, cwd, CLAIMS_LOCK_KEY);
    let mut reg = load(&path);
    reap(&mut reg, now, ttl);
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
    let _lock = RunLock::acquire(cfg, cwd, CLAIMS_LOCK_KEY);
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
    let _lock = RunLock::acquire(cfg, cwd, CLAIMS_LOCK_KEY);
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
    let _lock = RunLock::acquire(cfg, cwd, CLAIMS_LOCK_KEY);
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
    let _lock = RunLock::acquire(cfg, cwd, CLAIMS_LOCK_KEY);
    let mut reg = load(&path);
    reap(&mut reg, now, ttl);
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
    let _lock = RunLock::acquire(cfg, cwd, CLAIMS_LOCK_KEY);
    let mut reg = load(&path);
    reap(&mut reg, now, ttl);
    save(&path, &reg)?;
    Ok(reg)
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
    let _lock = RunLock::acquire(cfg, cwd, CLAIMS_LOCK_KEY);
    let mut reg = load(&path);
    reap(&mut reg, now, ttl);
    save(&path, &reg)?;

    let pending = backlog_pending();
    let entries = join_execution_state(&reg.task_claims, &pending);
    save(&execution_state_path(cfg, cwd), &entries)?;
    Ok(entries)
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
    fn stale_claim_past_heartbeat_ttl_is_reaped_and_reclaimable() {
        let tmp = make_tmp_dir("stale-hb");
        let cfg = make_cfg(&tmp);
        // A claim whose heartbeat is far in the past — the holding session died
        // without releasing. The pid is irrelevant to liveness (condukt is
        // ephemeral); staleness is decided purely by the heartbeat age.
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
        // now = ttl + 1 past the heartbeat → reaped, so a live run can take it.
        let now = cfg.stuck_ttl_secs as i64 + 1;
        let out = claim_files(&cfg, &tmp, "runB", None, &files(&["src/a.rs"]), now).unwrap();
        assert_eq!(out.claimed, vec!["src/a.rs".to_string()]);
        assert!(out.skipped.is_empty());
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
    fn stale_task_claim_is_reaped_on_load_and_reclaimable() {
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
        // now past the TTL of the heartbeat → reaped, so a live run can take it.
        let now = cfg.stuck_ttl_secs as i64 + 1;
        let out = claim_tasks(&cfg, &tmp, &files(&["hk-1"]), "runB", None, now, None).unwrap();
        assert_eq!(out.claimed, vec!["hk-1".to_string()]);
        assert!(out.skipped.is_empty());
        let live = active_claims(&cfg, &tmp, now).unwrap();
        assert_eq!(live.task_claims["hk-1"].run_id, "runB");
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
