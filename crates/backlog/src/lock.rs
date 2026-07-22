use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use harness_core::config::base_dir;

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
fn project_slug(project: &str) -> String {
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
    /// No lock file.
    None,
}

/// A lock is stale once its heartbeat is older than this many seconds without
/// a refresh. Mirrors condukt's `stuck_ttl_secs` / overwatch's
/// `LEASE_TTL_SECS` (both default to 1800s / 30min) for consistency across
/// the harness's cross-session staleness registries.
const LOCK_STALE_TTL_SECS: i64 = 1800;

fn is_stale(info: &LockInfo, now: i64) -> bool {
    now.saturating_sub(info.heartbeat_at) > LOCK_STALE_TTL_SECS
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
/// "is ANY project's driver active" scan (`status_any`). A missing/unreadable
/// `locks/` directory reads as "no locks" (fail-soft — mirrors every other
/// gate in this repo that treats an absent store as empty rather than erroring
/// the CLI invocation), since the directory is created lazily on first
/// `acquire` and its absence just means nobody has ever locked anything yet.
fn all_lock_files(dir: &Path) -> Vec<PathBuf> {
    match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|ext| ext == "lock"))
            .collect(),
        Err(_) => Vec::new(),
    }
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
/// check. Stale locks (whose heartbeat is older than the TTL) are reaped and
/// the create is retried a bounded number of times, so a dead holder never
/// blocks acquisition forever.
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
/// should be taken over. Unlike [`acquire_at`], which only reaps locks whose
/// owner pid is gone, this reaps the existing lock regardless of liveness.
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

/// Shared acquire implementation. `force = true` steals a live holder's lock
/// (the `--force` path); `force = false` only reaps confirmed-stale locks.
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
                    Some(_stale) => {
                        // Confirmed stale (readable, heartbeat past TTL) — reap and retry.
                        let _ = std::fs::remove_file(&path);
                        continue;
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
        None => LockStatus::None,
        Some(info) => {
            if is_stale(&info, now_unix()) {
                LockStatus::Stale(info)
            } else {
                LockStatus::Active(info)
            }
        }
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
/// most "alive" result across all lock files: an `Active` lock anywhere wins
/// over `Stale`, which wins over `None` (empty/missing `locks/` dir).
pub fn status_any_at(lock_dir: Option<&Path>) -> LockStatus {
    let dir = match lock_dir {
        Some(d) => locks_dir_for(d),
        None => locks_dir(),
    };
    let mut best = LockStatus::None;
    for path in all_lock_files(&dir) {
        let this = status_from_path(&path);
        best = match (&best, &this) {
            (LockStatus::Active(_), _) => best,
            (_, LockStatus::Active(_)) => this,
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

    #[test]
    fn stale_heartbeat_lock_is_reaped_and_stealable() {
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
        acquire_at("new-sess", pid, "proj", Some(d)).expect("should succeed over stale lock");
        match status_at("proj", Some(d)) {
            LockStatus::Active(i) => assert_eq!(i.session_id, "new-sess"),
            other => panic!("expected Active, got {other:?}"),
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
        acquire_at("new-sess", pid, "proj", Some(d)).expect("should succeed over stale lock");

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
        acquire_at("live", pid, "proj", Some(d)).expect("acquire must steal a stale lock");

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
            acquire_at("us", our_pid, "our-proj", Some(d))
                .expect("acquire must succeed over a stale pre-existing lock file");
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
}
