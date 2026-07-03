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
//! - **Stale reap**: a claim whose owner pid is gone, or whose heartbeat is older
//!   than the stuck-TTL, is reaped so a dead session never blocks others forever.
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

/// The on-disk registry: file path/glob -> its live holder.
pub type Registry = BTreeMap<String, Claim>;

/// `<state_dir>/<project-key>/claims.json` — beside the run-state files, so it is
/// per-project and unrelated projects never share a registry.
fn registry_path(cfg: &Config, cwd: &Path) -> PathBuf {
    cfg.state_dir
        .join(project_key(&repo_root(cwd)))
        .join("claims.json")
}

/// Fail-soft load: a missing or corrupt registry is treated as empty rather than
/// breaking the caller. (Corruption loses others' claims, but proceeding is safer
/// than aborting a state transition — the worst case degrades to today's
/// no-claim behavior.)
fn load(path: &Path) -> Registry {
    match std::fs::read_to_string(path) {
        Ok(txt) => serde_json::from_str(&txt).unwrap_or_default(),
        Err(_) => Registry::new(),
    }
}

/// Atomic write (temp + rename), mirroring `RunState::save`.
fn save(path: &Path, reg: &Registry) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(reg)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &json)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
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
    reg.retain(|_, c| !is_stale(c, now, ttl));
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
        let claimed_at = reg.get(f).map(|c| c.claimed_at).unwrap_or(now);
        reg.insert(
            f.clone(),
            Claim {
                run_id: run_id.to_string(),
                session_id: session_id.map(str::to_string),
                pid: std::process::id(),
                claimed_at,
                heartbeat_at: now,
            },
        );
        outcome.claimed.push(f.clone());
    }
    save(&path, &reg)?;
    Ok(outcome)
}

/// Release every file held by `run_id` (call on run completion / gate cleanup).
/// Returns the number of files released.
pub fn release_run(cfg: &Config, cwd: &Path, run_id: &str) -> Result<usize> {
    let path = registry_path(cfg, cwd);
    let _lock = RunLock::acquire(cfg, cwd, CLAIMS_LOCK_KEY);
    let mut reg = load(&path);
    let before = reg.len();
    reg.retain(|_, c| c.run_id != run_id);
    let removed = before - reg.len();
    save(&path, &reg)?;
    Ok(removed)
}

/// Release specific `files` held by `run_id` (call when a task reaches a terminal
/// status). Only removes files this run actually holds. Returns the count removed.
pub fn release_files(cfg: &Config, cwd: &Path, run_id: &str, files: &[String]) -> Result<usize> {
    let path = registry_path(cfg, cwd);
    let _lock = RunLock::acquire(cfg, cwd, CLAIMS_LOCK_KEY);
    let mut reg = load(&path);
    let before = reg.len();
    reg.retain(|k, c| !(c.run_id == run_id && files.contains(k)));
    let removed = before - reg.len();
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
    for c in reg.values_mut() {
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
            single_worktree: false,
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
        assert_eq!(live.len(), 1);
        assert_eq!(live["src/a.rs"].run_id, "runA");
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
        assert_eq!(live["src/a.rs"].run_id, "runA");
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
        assert_eq!(live["src/a.rs"].claimed_at, 100);
        assert_eq!(live["src/a.rs"].heartbeat_at, 200);
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
        assert!(active_claims(&cfg, &tmp, 101).unwrap().is_empty());
    }

    #[test]
    fn stale_claim_past_heartbeat_ttl_is_reaped_and_reclaimable() {
        let tmp = make_tmp_dir("stale-hb");
        let cfg = make_cfg(&tmp);
        // A claim whose heartbeat is far in the past — the holding session died
        // without releasing. The pid is irrelevant to liveness (condukt is
        // ephemeral); staleness is decided purely by the heartbeat age.
        let mut reg = Registry::new();
        reg.insert(
            "src/a.rs".to_string(),
            Claim {
                run_id: "ghost".into(),
                session_id: Some("dead-session".into()),
                pid: std::process::id(),
                claimed_at: 0,
                heartbeat_at: 0,
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
        let mut reg = Registry::new();
        reg.insert(
            "src/a.rs".to_string(),
            Claim {
                run_id: "other".into(),
                session_id: Some("live-elsewhere".into()),
                pid: 424242,
                claimed_at: 100,
                heartbeat_at: 100,
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
}
