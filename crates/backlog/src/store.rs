use anyhow::{anyhow, Context, Result};
use harness_core::verdict::Determination;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::task::{new_id, Task, STATUS_DONE, STATUS_FAILED, STATUS_PENDING};

/// CA-backlog-001: the atomic-claim reservation status written by
/// [`next_claim`]. Deliberately kept LOCAL to `store.rs` (not part of
/// `task::STATUSES`/the shared `--status` vocabulary) since it's an internal,
/// transient marker rather than a user-facing lifecycle state — a claimed
/// task is expected to resolve to `done`/`failed` shortly after via the
/// SAME id, or be reclaimed as stale (see [`CLAIM_STALE_SECS`]). It compares
/// unequal to `STATUS_PENDING`/`STATUS_FAILED`, so `Task::is_pending()`
/// correctly excludes a claimed task from `next`'s candidate pool without any
/// change to `is_pending` itself.
const STATUS_CLAIMED: &str = "claimed";

/// TOML ファイル全体のラッパー。[[task]] 配列を保持する。
#[derive(Debug, Default, Serialize, Deserialize)]
struct TasksFile {
    #[serde(default)]
    task: Vec<Task>,
}

/// tasks.toml から全タスクを読み込む。ファイル不在は空 Vec。
pub fn load(path: &Path) -> Result<Vec<Task>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let file: TasksFile =
        toml::from_str(&text).with_context(|| format!("failed to parse {}", path.display()))?;
    Ok(file.task)
}

/// A process-global monotonic counter that, combined with the pid and a
/// nanosecond timestamp, gives every [`save`] a UNIQUE temp filename.
///
/// CA-backlog-003: the old code used a single FIXED temp name
/// (`.tasks.toml.tmp`), so two concurrent DEGRADED writers (fail-soft callers
/// that bypassed `with_tasks_lock`) shared — and clobbered — the same temp
/// file: one writer's `std::fs::write` truncated the temp mid-write of the
/// other, and one `rename` moved a temp the other then tried to rename
/// (spurious ENOENT) → a lost update or a hard error. A pid+counter+nanos
/// suffix (mirroring `lock.rs`'s pid+nanos temp) gives each writer a private
/// temp so concurrent degraded writers never collide.
static SAVE_TMP_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Current time in nanoseconds since the Unix epoch (temp-filename entropy).
fn now_unix_nanos() -> u128 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// The ordered steps of a durable atomic [`save`], surfaced through the
/// [`DurabilitySyncer`] seam so a test can assert the fsync ORDERING that
/// CA-backlog-001 hardens — an ordering (temp fsync BEFORE the rename, parent-
/// dir fsync AFTER the rename) that is otherwise invisible to a black-box test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SaveStep {
    /// The serialized bytes were written in full to the temp file.
    WriteTmp,
    /// The temp file was fsync'd (must come BEFORE `Rename`).
    SyncTmp,
    /// The temp file was atomically renamed onto the target path.
    Rename,
    /// The parent directory was fsync'd (must come AFTER `Rename`).
    SyncDir,
}

/// Injectable durability sync operations for [`save`]. Production
/// ([`RealSyncer`]) performs real `fsync`s; a test can inject a recorder to
/// OBSERVE that `sync_file` runs before the rename and `sync_dir` runs after
/// it — the CA-backlog-001 ordering. The `on_step` hook lets an observer mark
/// the non-sync steps (write/rename) too so the FULL sequence can be asserted;
/// it defaults to a no-op for production.
trait DurabilitySyncer {
    fn on_step(&self, _step: SaveStep) {}
    /// fsync the fully-written temp file. Called BEFORE the rename. Errors
    /// propagate (a temp we can't durably flush must fail the save).
    fn sync_file(&self, f: &std::fs::File) -> std::io::Result<()>;
    /// fsync the parent directory. Called AFTER the rename. Best-effort.
    fn sync_dir(&self, dir: &Path);
}

/// Production syncer: real `fsync` on the temp file (before rename) and a
/// best-effort `fsync` on the parent directory (after rename), identical to the
/// inline behavior it replaced. Each method records its step through the no-op
/// `on_step` hook so the [`SaveStep`] variants are constructed on the
/// production path too (a recorder overrides `on_step` to observe them).
struct RealSyncer;

impl DurabilitySyncer for RealSyncer {
    fn sync_file(&self, f: &std::fs::File) -> std::io::Result<()> {
        self.on_step(SaveStep::SyncTmp);
        f.sync_all()
    }
    fn sync_dir(&self, dir: &Path) {
        self.on_step(SaveStep::SyncDir);
        if let Ok(dir_file) = std::fs::File::open(dir) {
            let _ = dir_file.sync_all();
        }
    }
}

/// Vec<Task> を tasks.toml に書き戻す (アトミック書き込み: 一時ファイル→rename)。
///
/// CA-backlog-001: the write is made DURABLE — the fully-written temp file is
/// `sync_all`'d (fsync) BEFORE the rename, and the parent directory is fsync'd
/// AFTER the rename — so a crash in the write window cannot leave `tasks.toml`
/// truncated/empty (losing every task). This mirrors `lock.rs`'s `sync_all`
/// durability. CA-backlog-003: the temp file has a UNIQUE per-writer name
/// (pid + monotonic counter + nanos), so two concurrent degraded writers never
/// share — and thus clobber — the same temp.
pub fn save(path: &Path, tasks: &[Task]) -> Result<()> {
    save_with_syncer(path, tasks, &RealSyncer)
}

/// The durable atomic-save core, generic over the [`DurabilitySyncer`] seam so
/// a test can OBSERVE the fsync ordering. Production drives it via [`save`]
/// with [`RealSyncer`]. The recorded step sequence is always
/// `WriteTmp → SyncTmp → Rename → SyncDir`.
fn save_with_syncer<S: DurabilitySyncer>(path: &Path, tasks: &[Task], syncer: &S) -> Result<()> {
    let file = TasksFile {
        task: tasks.to_vec(),
    };
    let text = toml::to_string_pretty(&file).context("failed to serialize tasks to TOML")?;

    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create directory {}", parent.display()))?;
        }
    }

    // CA-backlog-003: unique temp name (pid + monotonic counter + nanos) so
    // concurrent degraded writers get private temps and never collide.
    let base = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("tasks.toml");
    let counter = SAVE_TMP_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp_name = format!(
        ".{base}.tmp.{}.{}.{}",
        std::process::id(),
        counter,
        now_unix_nanos()
    );
    let tmp_path = path.with_file_name(tmp_name);

    // CA-backlog-001: write the temp file in full and fsync it BEFORE the
    // rename, so the rename can only ever publish a complete, durable file.
    // `create_new` (O_EXCL) makes the temp exclusively ours (the unique name
    // means it never pre-exists in practice; if a crashed writer left one, we
    // refuse to reuse it rather than silently overwrite).
    {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)
            .with_context(|| format!("failed to create tmp file {}", tmp_path.display()))?;
        f.write_all(text.as_bytes())
            .with_context(|| format!("failed to write tmp file {}", tmp_path.display()))?;
        syncer.on_step(SaveStep::WriteTmp);
        // Temp fsync BEFORE the rename (this is the durability ordering the
        // seam makes observable). Removing this call is exactly the pre-fix
        // regression the discriminating test guards against.
        syncer
            .sync_file(&f)
            .with_context(|| format!("failed to fsync tmp file {}", tmp_path.display()))?;
    }

    std::fs::rename(&tmp_path, path).with_context(|| {
        format!(
            "failed to rename {} -> {}",
            tmp_path.display(),
            path.display()
        )
    })?;
    syncer.on_step(SaveStep::Rename);

    // CA-backlog-001: fsync the parent directory so the rename (the new
    // directory entry) is itself durable — otherwise a crash right after the
    // rename could still lose the just-published file on some filesystems.
    // Best-effort: not all platforms/filesystems support fsync on a dir fd.
    if let Some(parent) = path.parent() {
        let dir = if parent.as_os_str().is_empty() {
            Path::new(".")
        } else {
            parent
        };
        // Parent-dir fsync AFTER the rename. Removing this call is the other
        // half of the pre-fix regression the discriminating test guards.
        syncer.sync_dir(dir);
    }

    Ok(())
}

// ---- tasks-file-scoped advisory lock ----------------------------------------
//
// A per-tasks-file advisory lock that serializes the read-modify-write critical
// section of the mutators (requeue_expired / add / mark_* / edit) so two
// concurrent callers on the SAME file cannot lost-update each other. `save` is
// already atomic (temp+rename, no torn file), but the load→modify→save WINDOW is
// unguarded, so without this lock the last writer clobbers a concurrent change.
//
// This is DELIBERATELY NOT `crate::lock`: that is the single GLOBAL `/flow`
// run.lock (exclusive, errors on a live holder). Wrapping the unattended
// SessionStart requeue in the global lock would skip requeue whenever a real
// `/flow` session held it, and make an interactive session see a phantom
// "another session active". This lock is keyed on the tasks-file path, is
// BLOCKING (bounded), and is fail-soft (degrades to unprotected best-effort
// rather than erroring), so it never breaks a turn.

/// Sibling lockfile path for a tasks file (e.g. `tasks.toml` -> `tasks.toml.lock`).
fn tasks_lock_path(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(".lock");
    PathBuf::from(s)
}

/// A lockfile older than this (by mtime) is treated as abandoned by a crashed
/// holder and reaped, so a dead holder never deadlocks the SessionStart hook.
///
/// CA-backlog-003: the critical section is USUALLY a single load-modify-save
/// (sub-millisecond), but `add`/`add_with_weight` can additionally shell out
/// to `condukt state is-claimed` via [`is_claimed_elsewhere`], which is bounded
/// at [`IS_CLAIMED_TIMEOUT`] (300ms) but can legitimately take close to that
/// long under load. The stale-reap window must stay a comfortable multiple
/// (not just ~16x) of BOTH that bound AND the blocking-acquire retry budget
/// below ([`TASKS_LOCK_MAX_ATTEMPTS`] × [`TASKS_LOCK_SLEEP`]), so a live
/// holder legitimately taking the full `is_claimed` bound is never mistaken
/// for a crashed one and reaped mid-critical-section by a waiting racer
/// (which would cause a lost update on tasks.toml — the "stale-lock reap
/// window race").
const TASKS_LOCK_STALE_SECS: u64 = 10;
/// Bounded blocking-acquire budget: attempts × sleep ≈ 8s worst case.
///
/// CA-backlog-003 originally sized this to comfortably exceed a SINGLE
/// legitimate holder's worst-case critical section ([`IS_CLAIMED_TIMEOUT`],
/// 300ms). That undercounted heavy contention: under N concurrent callers of
/// `add`/`add_with_weight` (each potentially paying the full 300ms inside the
/// lock via `is_claimed_elsewhere`), a waiter can queue behind several such
/// holders in a row — up to roughly N × 300ms serialized — not just one. At
/// N=20 (this crate's own `add_and_claim_no_lost_update_under_heavy_contention`
/// stress test: 10 adders + 10 claimers) that's ~6s worst case, which blew
/// through the old ~2s budget and made waiters degrade to unprotected
/// best-effort mid-queue — the lost-update race that test exists to catch.
/// The budget must stay a comfortable margin BELOW [`TASKS_LOCK_STALE_SECS`]
/// (10s) so a queued-behind-many-live-holders waiter is never mistaken for
/// one waiting on a crashed holder and made to reap+barge in early.
const TASKS_LOCK_MAX_ATTEMPTS: u32 = 1600;
const TASKS_LOCK_SLEEP: Duration = Duration::from_millis(5);

/// RAII guard for the tasks-file-scoped advisory lock. Removes the lockfile on
/// EVERY drop path — Ok return, Err return, or panic-unwind — so the lock is
/// always released.
struct TasksLockGuard {
    path: PathBuf,
}

impl Drop for TasksLockGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Is the lockfile obviously stale (older than [`TASKS_LOCK_STALE_SECS`])?
fn tasks_lock_is_stale(lock_path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(lock_path) else {
        return false;
    };
    let Ok(modified) = meta.modified() else {
        return false;
    };
    match modified.elapsed() {
        Ok(age) => age.as_secs() >= TASKS_LOCK_STALE_SECS,
        // Clock skew (mtime in the future): don't reap.
        Err(_) => false,
    }
}

/// Try to acquire the tasks-file-scoped advisory lock, BLOCKING (bounded) until
/// the current holder releases. Acquisition is atomic: `create_new` (O_EXCL)
/// means exactly one racer can create the lockfile; the loser retries. An
/// obviously-stale lockfile (crashed holder) is reaped so it never deadlocks.
///
/// Returns `Some(guard)` on success, or `None` if the lock could not be acquired
/// within the budget ([`TASKS_LOCK_MAX_ATTEMPTS`] × [`TASKS_LOCK_SLEEP`], sized
/// for queuing behind many concurrent legitimate holders — see that constant's
/// doc comment) — the caller must then degrade to a best-effort unprotected
/// operation (fail-soft) rather than erroring.
fn try_acquire_tasks_lock(path: &Path) -> Option<TasksLockGuard> {
    let lock_path = tasks_lock_path(path);
    if let Some(parent) = lock_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    for _ in 0..TASKS_LOCK_MAX_ATTEMPTS {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)
        {
            // We won the atomic create — we hold the lock.
            Ok(_f) => return Some(TasksLockGuard { path: lock_path }),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                // Held by someone else. Reap it if abandoned, else wait & retry.
                if tasks_lock_is_stale(&lock_path) {
                    let _ = std::fs::remove_file(&lock_path);
                    continue;
                }
                std::thread::sleep(TASKS_LOCK_SLEEP);
                continue;
            }
            // Unexpected FS error — degrade to best-effort (never error out).
            Err(_) => return None,
        }
    }
    None
}

/// Run `f` while holding the tasks-file-scoped advisory lock. If the lock cannot
/// be acquired within the bounded budget, degrade to running `f` UNPROTECTED
/// (fail-soft: never return `Err` purely because of lock contention). The guard
/// is dropped — and thus the lock released — on every exit path, including when
/// `f` returns `Err` or panics-unwinds.
fn with_tasks_lock<T>(path: &Path, f: impl FnOnce() -> Result<T>) -> Result<T> {
    // `_guard` is `Some` when we hold the lock, `None` on fail-soft degrade.
    // Either way it drops (releasing the lock if held) when this fn returns.
    let _guard = try_acquire_tasks_lock(path);
    f()
}

/// Like [`with_tasks_lock`], but the closure is TOLD whether the exclusive lock
/// is actually held (`true`) or the acquire degraded (`false`). Best-effort
/// mutators use plain [`with_tasks_lock`] and ignore this — a lost update under
/// contention is tolerable for them. Operations where mutual exclusion is a
/// CORRECTNESS requirement (a claim's read-modify-write, which double-dispatches
/// the SAME task to concurrent callers if it races) use this and FAIL CLOSED on
/// `false` rather than running the RMW unprotected. The guard drops (releasing
/// the lock if held) on every exit path, including `f` returning `Err` or
/// unwinding.
fn with_tasks_lock_aware<T>(path: &Path, f: impl FnOnce(bool) -> Result<T>) -> Result<T> {
    let guard = try_acquire_tasks_lock(path);
    f(guard.is_some())
}

/// タスクを追加して保存。生成した id を返す。weight は 0.0 (= 既定の優先順位)。
/// weight を明示したい呼び出し元は [`add_with_weight`] を使う。
///
/// バイナリ側は GitHub push 込みの [`add_with_weight_and_github_push`] を直接
/// 呼ぶため、この 0.0 既定ラッパーはテスト専用 (`#[cfg(test)]`)。
#[cfg(test)]
pub fn add(
    path: &Path,
    title: &str,
    project: &str,
    tags: Vec<String>,
    notes: &str,
    now: i64,
) -> Result<String> {
    add_with_weight(path, title, project, tags, notes, 0.0, false, now)
}

/// [`add`] に ordering weight を添えた版。weight は同一 priority 内の並び順を
/// 降順で駆動する (高い weight ほど next/list で先に来る)。compass opportunity
/// の weight をここへ供給すると、source 層のキュー順が opportunity の impact で
/// 決まる。weight=0.0 は legacy 既定で、従来の (priority, created_at) 順を保つ。
///
/// `force`: skip the duplicate-content guard (see [`check_duplicate`]) even
/// when an existing pending/failed task or a live cross-session claim already
/// holds this title+project's [`crate::task::hashkey`]. CLI surfaces this as
/// `backlog add --force`.
///
/// The binary now calls [`add_with_weight_and_github_push`] directly (it folds
/// in a fail-soft GitHub-issue push on top of this exact logic), so this
/// plain, push-free entry point is kept `#[cfg(test)]`-reachable: it remains
/// the direct target of this module's own weight-ordering/duplicate-guard/
/// concurrency tests, which don't need a `run` closure to exercise those
/// behaviors.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub fn add_with_weight(
    path: &Path,
    title: &str,
    project: &str,
    tags: Vec<String>,
    notes: &str,
    weight: f64,
    force: bool,
    now: i64,
) -> Result<String> {
    let is_bare =
        !(project.starts_with('/') || project.starts_with('.') || project.starts_with('~'));
    let CanonicalProject {
        label: project,
        unresolved: project_unresolved,
    } = canonicalize_project_with_marker(project);
    let project = &project;
    with_tasks_lock(path, || {
        let mut tasks = load(path)?;
        if is_bare {
            reject_ambiguous_bare_label(&tasks, project)?;
        }
        if !force {
            check_duplicate(&tasks, title, project)?;
        }
        let id = new_id(title, now);
        let task = Task {
            id: id.clone(),
            title: title.to_string(),
            project: project.to_string(),
            // Carry the undetermined-scope degrade onto the record itself, so
            // a guessed label never reads as a resolved one later.
            project_unresolved,
            tags,
            status: STATUS_PENDING.to_string(),
            notes: notes.to_string(),
            created_at: now,
            updated_at: now,
            defer_until: None,
            weight,
            issue_number: None,
            issue_url: None,
        };
        tasks.push(task);
        save(path, &tasks)?;
        Ok(id)
    })
}

/// Reject an add whose content [`crate::task::hashkey`] is already held by
/// EITHER (a) an existing task in `tasks` with status `pending`, `failed`, or
/// the in-progress `claimed` state (a `done` task with the same title does NOT
/// block a re-add), OR (b) a live cross-session claim reported by
/// `condukt state is-claimed --hashkey <h>`.
///
/// CA-backlog-002: `claimed` is included in (a). A task the local
/// [`next_claim`] has reserved is active, in-progress work; blocking only
/// `pending`/`failed` let a re-add slip through while the SAME task was
/// mid-flight (its cross-session ledger entry, checked in (b), is separate and
/// may be absent), spawning a duplicate of work already underway.
///
/// Fail-soft on (b): if the `condukt` binary is absent from PATH, or the
/// command errors or exits with anything other than 0 (claimed) / 1 (not
/// claimed), this is treated as "not claimed" — a missing or misbehaving
/// `condukt` must never block `backlog add`.
/// CA-backlog-007: a bare (non-path-shaped) `--project` label is not resolved
/// against anything at write time — `canonicalize_project` passes it through
/// unchanged — so it stays freely writable, and `bare_name_matches_path`
/// bridges it to ANY stored path sharing its basename with no registry
/// binding the label to one specific project. If a DIFFERENT project is
/// already known under a path whose basename equals this bare label, writing
/// it would create exactly the ambiguity that bridge can silently leak across
/// (fail closed rather than let a new ambiguous write in): refuse, naming the
/// colliding path so the caller can supply an unambiguous canonical path
/// instead. Reusing the SAME bare label already in the store is unaffected —
/// only a DIFFERENT, differently-pathed project sharing the basename trips
/// this.
fn reject_ambiguous_bare_label(tasks: &[Task], bare: &str) -> Result<()> {
    if let Some(colliding) = tasks.iter().find_map(|t| {
        (t.project != bare && Path::new(&t.project).file_name().is_some_and(|f| f == bare))
            .then_some(t.project.as_str())
    }) {
        return Err(anyhow!(
            "ambiguous project label {bare:?}: it shares a basename with the already-known project {colliding:?}, and no registry binds the bare label to a specific project; use an unambiguous canonical path instead"
        ));
    }
    Ok(())
}

fn check_duplicate(tasks: &[Task], title: &str, project: &str) -> Result<()> {
    let hk = crate::task::hashkey(title, project);

    if tasks.iter().any(|t| {
        crate::task::hashkey(&t.title, &t.project) == hk
            && matches!(
                t.status.as_str(),
                STATUS_PENDING | STATUS_FAILED | STATUS_CLAIMED
            )
    }) {
        return Err(anyhow!(
            "duplicate task rejected: an existing pending/failed/claimed task already has this content (hashkey {hk}); use --force to add anyway"
        ));
    }

    if is_claimed_elsewhere(&hk) {
        return Err(anyhow!(
            "duplicate task rejected: hashkey {hk} is claimed by a live cross-session run; use --force to add anyway"
        ));
    }

    Ok(())
}

/// CA-backlog-002/003: upper bound on how long [`is_claimed_elsewhere`] will
/// wait for the `condukt state is-claimed` subprocess before giving up and
/// treating it as "not claimed". This call runs INSIDE `with_tasks_lock`'s
/// critical section (via `check_duplicate`), so an unbounded wait (the old
/// `Command::output()`, which blocks until the child exits) lets a hung/slow
/// `condukt` process hold the tasks-file lock indefinitely — well past
/// [`TASKS_LOCK_STALE_SECS`] (10s), at which point a second process reaps the
/// "stale" lock and steals it mid-critical-section, causing a lost update on
/// tasks.toml. Bounding this well under that stale-reap window (over an order
/// of magnitude under 10s) means a hang here can never itself be the cause of
/// a lock-steal: the call always gives up long before the lock could look
/// stale. Separately, [`TASKS_LOCK_MAX_ATTEMPTS`] × [`TASKS_LOCK_SLEEP`] (the
/// blocking-acquire retry budget for a WAITING racer) is kept comfortably
/// ABOVE this bound, so a racer never gives up waiting — and falls back to
/// unprotected fail-soft execution — while the current holder is still
/// legitimately inside this bound.
const IS_CLAIMED_TIMEOUT: Duration = Duration::from_millis(300);
/// Poll interval while waiting for the subprocess to exit.
const IS_CLAIMED_POLL_INTERVAL: Duration = Duration::from_millis(5);

/// Fail-soft check: does a live cross-session claim (from any `condukt` run)
/// hold this hashkey? Shells out to `condukt state is-claimed --hashkey <h>`
/// (exit 0 = claimed, exit 1 = not claimed). Any other outcome — `condukt`
/// missing from PATH, spawn failure, unexpected exit code, OR the subprocess
/// failing to exit within [`IS_CLAIMED_TIMEOUT`] (CA-backlog-002) — is treated
/// as "not claimed" so a stale, absent, or hung `condukt` never blocks
/// `backlog add`, and — critically — never holds the tasks-file lock past a
/// bound well under the stale-reap window.
fn is_claimed_elsewhere(hashkey: &str) -> bool {
    let mut child = match std::process::Command::new("condukt")
        .arg("state")
        .arg("is-claimed")
        .arg("--hashkey")
        .arg(hashkey)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    run_with_bounded_wait(&mut child, IS_CLAIMED_TIMEOUT, IS_CLAIMED_POLL_INTERVAL)
        .map(|status| status.success())
        .unwrap_or(false)
}

/// Wait for `child` to exit, polling `try_wait` (non-blocking) instead of the
/// blocking `wait()`/`output()`, so the caller can give up after `timeout`
/// elapses. On timeout, best-effort `kill()` the child (so it doesn't linger
/// as an orphan) and return `None` — the caller treats `None` as "unknown,
/// fail open". Returns `Some(exit_status)` if the child exits within the
/// budget.
fn run_with_bounded_wait(
    child: &mut std::process::Child,
    timeout: Duration,
    poll_interval: Duration,
) -> Option<std::process::ExitStatus> {
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait(); // reap so it doesn't become a zombie
                    return None;
                }
                std::thread::sleep(poll_interval);
            }
            Err(_) => return None,
        }
    }
}

/// pending/failed タスクを優先度順 (priority() 昇順、同優先度は created_at 昇順) で返す。
/// tag_filter: Some(tag) なら tags にそのタグを含むものだけ。
/// project_filter: Some(project) ならプロジェクトが一致するものだけ (repo_root との比較)。
/// defer_until が未来のタスク (is_deferred) はスキップする。
///
/// CA-backlog-001: this is a PURE READ — no lock, no mutation. It returns a
/// clone of a task left in `pending`, so two concurrent callers can be handed
/// the identical task before either acts on it. This is the pre-existing,
/// still-supported default behavior for callers that layer their own
/// external claim/dedup on top (e.g. `/flow`'s `condukt state claim-task`).
/// Callers who need backlog itself to guarantee at most one claimant should
/// use [`next_claim`] instead.
pub fn next(
    path: &Path,
    tag_filter: Option<&str>,
    project_filter: Option<&str>,
) -> Result<Option<Task>> {
    let now = now_unix();
    let tasks = load(path)?;
    // CA-backlog-006: canonicalize the filter the same way `add_with_weight`
    // canonicalizes a stored project, so a raw (possibly symlinked) path
    // still matches its already-canonical stored form. See `list` for the
    // full rationale.
    let project_filter = project_filter.map(canonicalize_project);
    Ok(pick_next(&tasks, now, tag_filter, project_filter.as_deref()).map(|t| (*t).clone()))
}

/// Selects the single highest-priority eligible task from `tasks` (shared by
/// [`next`] and [`next_claim`] so both use IDENTICAL candidate-selection and
/// ordering logic).
///
/// Unlike [`list`], this does NOT exempt `project_unresolved` tasks from
/// `project_filter`. The two commands differ in what a wrong answer costs: a
/// listing that hides a task loses information (so an unresolved label must
/// not filter it out), whereas `next` HANDS a task to a driver that will act
/// on it — dispatching work on a guessed project label could run it in the
/// wrong checkout entirely. Excluding an unresolved task here is the
/// restrictive side, and it is not silent: the task is still visible (and
/// marked) in `list`.
fn pick_next<'a>(
    tasks: &'a [Task],
    now: i64,
    tag_filter: Option<&str>,
    project_filter: Option<&str>,
) -> Option<&'a Task> {
    let mut candidates: Vec<&Task> = tasks
        .iter()
        .filter(|t| t.is_pending())
        .filter(|t| !t.is_deferred(now))
        .filter(|t| match tag_filter {
            Some(tag) => t.tags.iter().any(|tg| tg == tag),
            None => true,
        })
        .filter(|t| match project_filter {
            Some(proj) => project_matches(&t.project, proj),
            None => true,
        })
        .collect();

    candidates.sort_by(|a, b| queue_order(a, b));
    candidates.into_iter().next()
}

/// CA-backlog-001: a claim older than this (by `updated_at`) is treated as
/// abandoned (the claimant crashed or was killed before calling `done`/`fail`)
/// and is eligible to be reclaimed by a fresh [`next_claim`] call, so a dead
/// claimant never permanently removes a task from the queue.
pub const CLAIM_STALE_SECS: i64 = 3600;

/// Atomically select the next eligible task AND mark it `claimed` in the same
/// tasks-file-lock critical section, so a second concurrent `next_claim` call
/// — even one racing within the same window — cannot observe and return the
/// same task before the first call's claim is persisted (CA-backlog-001).
///
/// This is opt-in (`backlog next --claim`); the plain [`next`] (no `--claim`)
/// keeps its pre-existing pure-read behavior for existing callers, so this
/// does not change default behavior in an incompatible way.
///
/// A `claimed` task is excluded from the candidate pool (`is_pending()` is
/// false for `claimed`), UNLESS its claim is older than [`CLAIM_STALE_SECS`],
/// in which case it is treated as eligible again (stale-claim reclaim) so a
/// crashed claimer can't strand a task forever.
///
/// Returns the claimed task (with its in-memory `status` already updated to
/// `claimed` to match what was persisted), or `None` if no eligible task
/// exists.
pub fn next_claim(
    path: &Path,
    tag_filter: Option<&str>,
    project_filter: Option<&str>,
) -> Result<Option<Task>> {
    // CA-backlog-006: same read-side canonicalization as `next`/`list`.
    let project_filter = project_filter.map(canonicalize_project);
    with_tasks_lock_aware(path, |locked| {
        claim_next_locked(path, tag_filter, project_filter.as_deref(), locked)
    })
}

/// The claim read-modify-write, split out so the fail-closed contract is
/// deterministically testable. `locked` reports whether the exclusive tasks-lock
/// is held.
///
/// **FAIL CLOSED.** Without the lock we cannot guarantee this claim's
/// read-modify-write is mutually exclusive, and an unprotected claim reads the
/// same pending task in two concurrent callers and marks it `claimed` in both —
/// dispatching ONE task to multiple processes (the silent double-claim this
/// guard exists to prevent). So when `!locked` we DECLINE — return `Ok(None)`,
/// "nothing claimed right now" — rather than race an unprotected RMW. Declining
/// is the safe degrade (the caller simply retries on its next tick; contention
/// is bounded by the acquire budget), and it keeps to the doctrine that when
/// safety cannot be guaranteed the safe action is to NOT act.
fn claim_next_locked(
    path: &Path,
    tag_filter: Option<&str>,
    project_filter: Option<&str>,
    locked: bool,
) -> Result<Option<Task>> {
    if !locked {
        return Ok(None);
    }
    {
        let now = now_unix();
        let mut tasks = load(path)?;

        // Build the pending-or-reclaimable-stale-claim candidate pool inline
        // (can't reuse `pick_next`'s `is_pending()`-only filter as-is, since a
        // fresh claim must NOT be reclaimable, only a stale one).
        let winner_id = {
            let mut candidates: Vec<&Task> = tasks
                .iter()
                .filter(|t| {
                    t.is_pending()
                        || (t.status == STATUS_CLAIMED
                            && now.saturating_sub(t.updated_at) >= CLAIM_STALE_SECS)
                })
                .filter(|t| !t.is_deferred(now))
                .filter(|t| match tag_filter {
                    Some(tag) => t.tags.iter().any(|tg| tg == tag),
                    None => true,
                })
                .filter(|t| match project_filter {
                    Some(proj) => project_matches(&t.project, proj),
                    None => true,
                })
                .collect();
            candidates.sort_by(|a, b| queue_order(a, b));
            candidates.first().map(|t| t.id.clone())
        };

        let Some(id) = winner_id else {
            return Ok(None);
        };

        let task = tasks
            .iter_mut()
            .find(|t| t.id == id)
            .expect("winner_id came from tasks");
        task.status = STATUS_CLAIMED.to_string();
        task.updated_at = now;
        let claimed = task.clone();
        save(path, &tasks)?;
        Ok(Some(claimed))
    }
}

/// The deterministic source-layer queue order:
///   1. priority() ascending (p0 before p1 …),
///   2. weight descending (higher opportunity impact surfaces first),
///   3. created_at ascending (older first — the original FIFO tie-break).
///
/// `f64::total_cmp` gives a total order over weight (no NaN panics). With all
/// weights at the 0.0 default this collapses to the legacy (priority,
/// created_at) order, so existing tasks.toml files are unaffected.
fn queue_order(a: &Task, b: &Task) -> std::cmp::Ordering {
    a.priority()
        .cmp(&b.priority())
        .then(b.weight.total_cmp(&a.weight))
        .then(a.created_at.cmp(&b.created_at))
}

/// defer_until <= now のタスクの defer_until を None にクリアして status を "pending" に戻す。
/// 加えて、TTL を超えた stale な `claimed` タスクも "pending" に戻す (CA-backlog-005)。
/// 変更したタスクの件数を返す。
pub fn requeue_expired(path: &Path, now: i64) -> Result<usize> {
    // Serialize the load-modify-save against concurrent mutators on the same
    // file. Fail-soft: if the scoped lock cannot be acquired, `with_tasks_lock`
    // still runs the body unprotected, so this never starts returning Err where
    // it previously returned Ok, and never blocks the SessionStart hook.
    with_tasks_lock(path, || {
        let mut tasks = load(path)?;
        let mut count = 0usize;
        for task in tasks.iter_mut() {
            let mut changed = false;
            if let Some(defer_until) = task.defer_until {
                if defer_until <= now {
                    task.defer_until = None;
                    task.status = STATUS_PENDING.to_string();
                    changed = true;
                }
            }
            // CA-backlog-005: a stale `claimed` task — its claimant crashed or
            // was killed before calling `done`/`fail` and its claim is older
            // than CLAIM_STALE_SECS — is rescued back to `pending` here too, so
            // plain `next` and the unattended SessionStart `requeue_expired`
            // (not ONLY `next_claim`) can resurface it. Otherwise a crashed
            // claimant strands its task in `claimed` indefinitely.
            if task.status == STATUS_CLAIMED
                && now.saturating_sub(task.updated_at) >= CLAIM_STALE_SECS
            {
                task.status = STATUS_PENDING.to_string();
                changed = true;
            }
            if changed {
                task.updated_at = now;
                count += 1;
            }
        }
        if count > 0 {
            save(path, &tasks)?;
        }
        Ok(count)
    })
}

/// id で特定のタスクを done に更新して保存。見つからなければエラー。
///
/// CA-backlog-003: idempotent — if the task is ALREADY `done` (e.g. a
/// duplicate/retried call after a caller-side timeout or crash-and-restart
/// racing with its own earlier completion), this is a no-op that returns
/// `Ok(())` without touching `updated_at` again, rather than silently
/// re-stamping the completion time. This makes at-least-once callers
/// (retry-on-timeout, `/flow` re-driving a step after a partial failure) safe
/// to call twice for the same id without side effects.
pub fn mark_done(path: &Path, id: &str) -> Result<()> {
    with_tasks_lock(path, || {
        let mut tasks = load(path)?;
        let task = tasks
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or_else(|| anyhow!("task not found: {}", id))?;
        if task.status == STATUS_DONE {
            // Already done — idempotent no-op, nothing to persist.
            return Ok(());
        }
        task.status = STATUS_DONE.to_string();
        // updated_at はシステム時刻で更新（呼び出し元が now を持たないため現在時刻を使う）
        task.updated_at = now_unix();
        save(path, &tasks)
    })
}

/// id で特定のタスクを failed に更新。reason を notes に追記。
/// defer_until を now + 172800 (2日) に設定してタスクを一時保留にする。
///
/// CA-backlog-003: idempotent — if the task is ALREADY `failed`, this is a
/// no-op that returns `Ok(())` without re-appending `reason` to `notes` or
/// pushing `defer_until` further into the future again, rather than letting a
/// duplicate/retried call accumulate repeated notes or keep re-deferring the
/// task. This makes at-least-once callers safe to call twice for the same id
/// without side effects.
pub fn mark_failed(path: &Path, id: &str, reason: Option<&str>) -> Result<()> {
    with_tasks_lock(path, || {
        let mut tasks = load(path)?;
        let task = tasks
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or_else(|| anyhow!("task not found: {}", id))?;
        if task.status == STATUS_FAILED {
            // Already failed — idempotent no-op, nothing to persist.
            return Ok(());
        }
        task.status = STATUS_FAILED.to_string();
        if let Some(r) = reason {
            if task.notes.is_empty() {
                task.notes = r.to_string();
            } else {
                task.notes.push('\n');
                task.notes.push_str(r);
            }
        }
        let now = now_unix();
        task.defer_until = Some(now + 172_800);
        task.updated_at = now;
        save(path, &tasks)
    })
}

/// フィールドの一部を更新して保存。None のフィールドは変更しない。
///
/// CA-backlog-004: an unknown `status` is REJECTED (validated against the same
/// [`crate::task::STATUSES`] vocabulary that `list` warns on) BEFORE any write.
/// Previously `edit --status open` wrote the raw typo through, stranding the
/// task out of `next`/`next_claim` (neither `open` nor any non-vocabulary
/// value is `is_pending()`) with no path back — `requeue_expired` only rescues
/// deferred/stale-claimed tasks, so the typo'd task was lost. Rejecting up
/// front keeps the task in its last valid state.
pub fn edit(
    path: &Path,
    id: &str,
    title: Option<&str>,
    tags: Option<Vec<String>>,
    notes: Option<&str>,
    status: Option<&str>,
) -> Result<()> {
    with_tasks_lock(path, || {
        // CA-backlog-004: reject an unknown --status up front (same validation
        // `list` uses) so a typo can never be persisted and strand the task.
        if let Some(w) = crate::task::status_warning(status) {
            return Err(anyhow!("{w}"));
        }
        let mut tasks = load(path)?;
        let task = tasks
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or_else(|| anyhow!("task not found: {}", id))?;
        if let Some(v) = title {
            task.title = v.to_string();
        }
        if let Some(v) = tags {
            task.tags = v;
        }
        if let Some(v) = notes {
            task.notes = v.to_string();
        }
        if let Some(v) = status {
            task.status = v.to_string();
        }
        task.updated_at = now_unix();
        save(path, &tasks)
    })
}

/// Like [`add_with_weight`], but additionally attempts a fail-soft, one-way
/// push of the new task to GitHub as an issue (Phase 1: `backlog add` ->
/// `gh issue create`), recording `issue_number`/`issue_url` on the persisted
/// `Task` when it succeeds.
///
/// `remote_url` is the project's git remote URL (or `""`/anything
/// non-github.com to skip the push entirely — see
/// [`crate::github::is_github_remote`]). `run` is the SAME kind of injectable
/// command runner `crate::github::decide_issue_create` expects
/// (`Fn(&[&str]) -> Option<(bool, String)>`); the real `gh` spawn belongs at
/// the IO boundary in `main.rs`, never here, so this function stays fully
/// unit-testable with a fake closure and never spawns a process itself.
///
/// Fail-soft is mandatory: whatever `crate::github::decide_issue_create`
/// decides — non-GitHub remote, `gh` absent, or `gh issue create` failing
/// (non-zero exit) — `add_with_weight`'s local-only add still succeeds; only
/// a `Created{url}` outcome additionally sets `issue_number`/`issue_url`
/// (parsed via [`crate::github::parse_issue_number`]) before the SAME save.
/// All of `add_with_weight`'s existing guards (bare-label ambiguity rejection,
/// duplicate-content guard, tasks-file locking) are reused unchanged — this
/// function performs the identical read-modify-write, just with the GitHub
/// push folded into the same critical section before `save`.
#[allow(clippy::too_many_arguments)]
pub fn add_with_weight_and_github_push<R: Fn(&[&str]) -> Option<(bool, String)>>(
    path: &Path,
    title: &str,
    project: &str,
    tags: Vec<String>,
    notes: &str,
    weight: f64,
    force: bool,
    now: i64,
    remote_url: &str,
    run: R,
) -> Result<String> {
    let is_bare =
        !(project.starts_with('/') || project.starts_with('.') || project.starts_with('~'));
    let CanonicalProject {
        label: project,
        unresolved: project_unresolved,
    } = canonicalize_project_with_marker(project);
    let project = &project;
    with_tasks_lock(path, || {
        let mut tasks = load(path)?;
        if is_bare {
            reject_ambiguous_bare_label(&tasks, project)?;
        }
        if !force {
            check_duplicate(&tasks, title, project)?;
        }
        let id = new_id(title, now);
        let mut task = Task {
            id: id.clone(),
            title: title.to_string(),
            project: project.to_string(),
            // Same marker as `add_with_weight`: every write path records the
            // undetermined-scope degrade, otherwise the binary's real entry
            // point (this one) would be the fail-open hole.
            project_unresolved,
            tags,
            status: STATUS_PENDING.to_string(),
            notes: notes.to_string(),
            created_at: now,
            updated_at: now,
            defer_until: None,
            weight,
            issue_number: None,
            issue_url: None,
        };

        // Fail-soft GitHub push: never abort the add on a non-GitHub remote,
        // an absent `gh`, or a failed `gh issue create` — only a genuine
        // `Created{url}` populates issue_number/issue_url.
        if let crate::github::IssueOutcome::Created { url } =
            crate::github::decide_issue_create(remote_url, title, notes, &run)
        {
            task.issue_number = crate::github::parse_issue_number(&url);
            task.issue_url = Some(url);
        }

        tasks.push(task);
        save(path, &tasks)?;
        Ok(id)
    })
}

/// タスク一覧を返す。フィルタは all None で全件。
///
/// One deliberate exception to `project_filter`: a task marked
/// `project_unresolved` (written while its project identity was UNDETERMINED,
/// so its stored label is a fallback guess) is NOT filtered out on that
/// guessed label — it is listed regardless, and `main.rs` renders it as
/// `unresolved` so a reader can tell it apart. Filtering it would hide the
/// task from every checkout with no diagnostic. Tasks without the marker are
/// filtered exactly as before.
pub fn list(
    path: &Path,
    tag_filter: Option<&str>,
    project_filter: Option<&str>,
    status_filter: Option<&str>,
) -> Result<Vec<Task>> {
    let tasks = load(path)?;
    // CA-backlog-006: `add_with_weight` canonicalizes a path-shaped project
    // (via `canonicalize_project`, which resolves symlinks through
    // `resolve_repo_root`) before storing it, but callers pass this filter
    // raw — CLI `--project` and SessionStart's `repo_root()` both hand in an
    // uncanonicalized cwd-derived path. Canonicalizing here, once, at the read
    // boundary, closes that drift for every caller of `list`/`next`/
    // `next_claim` without requiring each of them to resolve symlinks
    // themselves.
    let project_filter = project_filter.map(canonicalize_project);
    let project_filter = project_filter.as_deref();
    let mut result: Vec<Task> = tasks
        .into_iter()
        .filter(|t| match tag_filter {
            Some(tag) => t.tags.iter().any(|tg| tg == tag),
            None => true,
        })
        .filter(|t| match project_filter {
            // A task whose `project` was recorded UNDETERMINED (see
            // `Task::project_unresolved`) is exempt from this filter: its
            // label is a guess, so a non-match proves nothing about where the
            // task belongs, and dropping it here would turn "could not
            // determine the project" into "there is nothing here" — the
            // collapse CLAUDE.md §3 forbids. Note this exempts only the
            // explicitly-marked tasks; every other task is filtered exactly as
            // before, so this is not a weakening of the filter itself.
            // `main.rs` renders the exempted rows as `unresolved` so they are
            // never mistaken for tasks that genuinely scope here.
            Some(proj) => t.project_unresolved || project_matches(&t.project, proj),
            None => true,
        })
        .filter(|t| match status_filter {
            Some(s) => t.status == s,
            None => true,
        })
        .collect();
    // Same weight-aware order as `next`, so `list` surfaces tasks in the order
    // they would actually be picked (priority → weight desc → created_at).
    result.sort_by(queue_order);
    Ok(result)
}

// ---- helpers ----------------------------------------------------------------

/// The outcome of normalizing a `--project` value: the canonical label, plus
/// whether that label was actually RESOLVED or is a fallback GUESS substituted
/// for an identity that could not be determined. Two answers, because
/// collapsing them into the label alone is precisely what made a guessed
/// project indistinguishable from a resolved one downstream (CLAUDE.md §3).
/// Produced by [`canonicalize_project_with_marker`].
pub(crate) struct CanonicalProject {
    /// The project label to store (or filter with). When `unresolved` is set
    /// this is the plain git-toplevel fallback, NOT a resolved identity.
    pub(crate) label: String,
    /// **Definition, and the whole contract of this field: `true` exactly when
    /// a GUESS WAS SUBSTITUTED** — this checkout's identity could not be
    /// determined, so `label` holds a fallback that may not be what a peer
    /// checkout later filters by.
    ///
    /// It is deliberately NOT "anything unusual happened". In particular a
    /// path that is DEFINITIVELY ABSENT on this machine does not set it: no
    /// guess was substituted there, the caller's own explicit label was
    /// adopted verbatim, and every checkout normalizes it identically, so it
    /// can never go missing the way a guess can. Conflating absent with
    /// undetermined would flag every legitimate cross-machine label (this
    /// repo's own store holds `C:/Users/.../harness`) and leave the marker
    /// carrying no signal — the `Absent` / `Undetermined` / `Known` distinction
    /// CLAUDE.md §3 names explicitly.
    ///
    /// Never set for a bare label or a successfully resolved path.
    pub(crate) unresolved: bool,
}

/// Normalize a `--project` value before it is stored (backlog id b92a7a77):
/// a path-shaped value (starts with `/`, `.`, or `~`) is resolved to its
/// canonical git repo root — mirroring `harness_core::discovery`'s own
/// resolve-then-key strategy — so `--project "$PWD"` from any subdirectory of
/// a repo, or from a sibling worktree, always lands on the same string
/// (closing the drift where different callers landed at different
/// subdir/worktree paths for the SAME project). A bare short label (no
/// separator, e.g. `"aegis"`) is passed through unchanged: it isn't a real
/// path, so resolving it against this process's cwd would risk landing on an
/// unrelated directory that coincidentally shares the name. `project_matches`
/// separately bridges bare labels already in the store against absolute
/// paths at read time.
///
/// A path-shaped value that names a LINKED WORKTREE (its `.git` is a file, not
/// a directory) is further normalized to the MAIN working tree it belongs to
/// via [`canonical_project_id`] — so `--project <worktree>` and
/// `--project <that worktree's main tree>` land on the identical stored
/// string, and a task recorded from either checkout is visible from the
/// other's default `list` scope (which resolves the SAME way — see
/// `main.rs`'s `List` handler). This normalization is deliberately
/// best-effort: when the worktree's main tree cannot actually be resolved
/// (`canonical_project_id` returns `Undetermined` — e.g. a dangling `.git`
/// link), `add`/`edit` must still succeed (write-time canonicalization has
/// never been allowed to block a write), so this falls back to the plain
/// git-toplevel resolution `resolve_repo_root` already provided — but NOT
/// silently: a diagnostic naming the reason goes to stderr first, since a
/// task stored under this unresolved fallback label may not match the
/// canonicalized string a peer checkout later filters by, and so may not
/// appear in that checkout's default (filtered) `list` (see CLAUDE.md §3 —
/// an undetermined outcome must never look identical to a resolved one).
///
/// **ABSENT IS NOT UNDETERMINED.** A path that does not exist on this machine
/// is checked FIRST and is not an undetermined scope at all: there is nothing
/// here to resolve, so no guess is substituted — the caller's explicit label
/// is adopted (via the same `resolve_repo_root` lexical normalization both the
/// write and the read side apply, so the two always agree) and reported as
/// resolved, silently. This is the normal, supported cross-machine case: the
/// store is shared, and a label like `C:/Users/.../harness` or another
/// checkout's path is meaningful even where it cannot be stat'ed. Emitting the
/// undetermined-scope warning here would be prose that contradicts the code
/// (nothing was "falling back to an unresolved label"), and marking it would
/// flag every such task forever.
///
/// The discriminator is "existence was DISPROVEN", not "existence could not be
/// checked": a `symlink_metadata` error other than `NotFound` (permissions, an
/// IO fault) means we cannot tell whether the path is there, which is itself an
/// undetermined outcome and resolves to the RESTRICTIVE side (marked, with a
/// diagnostic) rather than being waved through as absent.
/// The stderr diagnostic alone is NOT enough, though: it is gone the moment
/// the command exits, while the guessed label lives on in the store looking
/// exactly like a resolved one. So this returns the degrade as DATA —
/// [`CanonicalProject::unresolved`] — and every write path persists it onto
/// the task (`Task::project_unresolved`), which is what lets `list` keep the
/// task visible and render it as a guess instead of silently filtering it out.
pub(crate) fn canonicalize_project_with_marker(project: &str) -> CanonicalProject {
    if !(project.starts_with('/') || project.starts_with('.') || project.starts_with('~')) {
        return CanonicalProject {
            label: project.to_string(),
            unresolved: false,
        };
    }
    let expanded = harness_core::config::expand_tilde(project);

    // ABSENT vs UNDETERMINED, decided before any identity resolution is
    // attempted (see this function's doc comment). `symlink_metadata` — not
    // `exists()` — because `exists()` follows symlinks and so answers "false"
    // for a DANGLING symlink, which is a path that is really there and whose
    // identity genuinely cannot be resolved. That case must reach the
    // undetermined branch below, not be mistaken for absent.
    match std::fs::symlink_metadata(&expanded) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Existence DISPROVEN: nothing to resolve, nothing guessed.
            return CanonicalProject {
                label: repo_toplevel_label(&expanded),
                unresolved: false,
            };
        }
        Err(e) => {
            // Existence could not be CHECKED (permissions, IO fault). "Cannot
            // determine" is not "absent" — resolve to the restrictive side.
            eprintln!(
                "warning: could not resolve project scope for {} (could not determine whether it \
                 exists: {e}); falling back to the unresolved repo-toplevel label — this task may \
                 not appear under the default (filtered) `list` from other checkouts",
                expanded.display(),
            );
            return CanonicalProject {
                label: repo_toplevel_label(&expanded),
                unresolved: true,
            };
        }
        Ok(_) => {}
    }

    match canonical_project_id(&expanded) {
        Determination::Known(root) => CanonicalProject {
            label: root.to_string_lossy().into_owned(),
            unresolved: false,
        },
        Determination::Undetermined(why) => {
            eprintln!(
                "warning: could not resolve project scope for {} ({}); falling back to the \
                 unresolved repo-toplevel label — this task may not appear under the default \
                 (filtered) `list` from other checkouts",
                expanded.display(),
                why.as_str()
            );
            CanonicalProject {
                label: repo_toplevel_label(&expanded),
                unresolved: true,
            }
        }
    }
}

/// The plain git-toplevel fallback label, shared by every branch of
/// [`canonicalize_project_with_marker`] that does not have a resolved identity
/// to use. For an absent path `resolve_repo_root` bottoms out in a purely
/// lexical normalization (no stat, no `git`), which is what makes the write
/// side and the read side agree on the SAME string for a path neither of them
/// can see.
fn repo_toplevel_label(expanded: &Path) -> String {
    harness_core::discovery::resolve_repo_root(expanded)
        .to_string_lossy()
        .into_owned()
}

/// Label-only view of [`canonicalize_project_with_marker`], for the callers
/// that consume a canonical label WITHOUT persisting it: the read-side
/// `--project` filters in [`list`]/[`next`]/[`next_claim`] and
/// `lock::project_slug`. Dropping the `unresolved` bit is safe *only* here —
/// nothing durable is written under the guessed label by these callers, and
/// the stderr diagnostic from the shared implementation still fires — so
/// "cannot determine" never reaches a persisted record looking resolved.
/// Any NEW caller that stores what this returns must use
/// [`canonicalize_project_with_marker`] instead.
pub(crate) fn canonicalize_project(project: &str) -> String {
    canonicalize_project_with_marker(project).label
}

/// This checkout's canonical repo identity (CA-backlog-008 / CLAUDE.md §8's
/// worktree-isolation follow-through): `Known` when project scope was
/// actually resolved; `Undetermined` when resolution was attempted but could
/// not reach a conclusion. This is deliberately NEVER a silent fallback to
/// the raw, unresolved `cwd` — the caller decides what an undetermined scope
/// means (`canonicalize_project` degrades gracefully; `main.rs`'s `list`
/// default scope hard-errors, since an empty filtered result there is
/// indistinguishable from a genuinely empty queue — see that call site).
///
/// Filesystem-only (no `git` subprocess), mirroring
/// `harness_core::trust::resolve`'s worktree-inheritance algorithm exactly —
/// the SAME verified (not assumed) `gitdir:` walk, ported here because that
/// function is private to `harness-core`.
///
/// Resolution:
///   1. Nearest ancestor of `cwd` (inclusive) holding a `.git` entry — the
///      IDENTICAL scan `config::repo_root` uses to pick the store's
///      LOCATION (a separate concern: that scan decides which file
///      `tasks.toml` is; this one decides what project IDENTITY a resolved
///      checkout maps to). No ancestor found: this checkout isn't inside any
///      repo at all — a documented, intentional case (see
///      `Config::tasks_path_for`'s doc comment on its own legacy `~/.backlog`
///      fallback), not a "cannot determine" defect — so this is `Known(cwd)`
///      (canonicalized) rather than `Undetermined`, UNLESS canonicalizing
///      `cwd` itself fails (a genuine IO "could not determine").
///   2. `.git` is a DIRECTORY: this checkout IS a main working tree; its own
///      canonicalized root is the identity.
///   3. `.git` is a FILE (a linked worktree): resolve to the MAIN working
///      tree it belongs to via [`main_worktree_of`]. A dangling worktree
///      link, an unusual `GIT_DIR` layout, or an unreadable gitfile all make
///      that resolution fail — and are exactly the "cannot determine" case
///      this function exists to surface as `Undetermined` rather than
///      silently falling back to this checkout's own (wrong) path.
pub(crate) fn canonical_project_id(cwd: &Path) -> Determination<PathBuf> {
    let Some(root) = crate::config::repo_root(cwd) else {
        return match cwd.canonicalize() {
            Ok(c) => Determination::known(c),
            Err(e) => Determination::undetermined(format!(
                "{} is outside any repo and could not itself be canonicalized: {e}",
                cwd.display()
            )),
        };
    };
    let dot_git = root.join(".git");
    match std::fs::symlink_metadata(&dot_git) {
        Ok(meta) if meta.is_file() => match main_worktree_of(&root) {
            Some(main) => match main.canonicalize() {
                Ok(c) => Determination::known(c),
                Err(e) => Determination::undetermined(format!(
                    "{}'s main working tree {} could not be canonicalized: {e}",
                    root.display(),
                    main.display()
                )),
            },
            None => Determination::undetermined(format!(
                "{} is a linked worktree whose main working tree could not be resolved \
                 from its `.git` file (dangling link or unusual layout)",
                root.display()
            )),
        },
        Ok(_) => match root.canonicalize() {
            Ok(c) => Determination::known(c),
            Err(e) => Determination::undetermined(format!(
                "repo root {} could not be canonicalized: {e}",
                root.display()
            )),
        },
        Err(e) => Determination::undetermined(format!("could not stat {}: {e}", dot_git.display())),
    }
}

/// If `root` is a **linked** git worktree, the main working tree it belongs
/// to. Ported from `harness_core::trust::main_worktree_of` (private to that
/// crate) — see its doc comment for the full rationale; this is the identical
/// algorithm: read the `gitdir:` gitfile, walk up
/// `.../<common-git-dir>/worktrees/<name>`, and VERIFY the inference (not
/// assume it from path shape alone) by canonicalizing both the common git dir
/// and the candidate main tree's own `.git` and requiring them to be equal.
/// Any IO failure, an unusual layout, or a `GIT_DIR` that doesn't exist
/// (a dangling worktree link) all yield `None` — the restricted side, since
/// [`canonical_project_id`] treats `None` as "cannot determine".
fn main_worktree_of(root: &Path) -> Option<PathBuf> {
    let dot_git = root.join(".git");
    if !std::fs::symlink_metadata(&dot_git).ok()?.is_file() {
        return None;
    }
    let text = std::fs::read_to_string(&dot_git).ok()?;
    let target = text
        .lines()
        .find_map(|l| l.trim().strip_prefix("gitdir:"))?
        .trim();
    if target.is_empty() {
        return None;
    }
    let target = Path::new(target);
    let git_dir = if target.is_absolute() {
        target.to_path_buf()
    } else {
        root.join(target)
    };

    // .../<common-git-dir>/worktrees/<name>
    let worktrees = git_dir.parent()?;
    if worktrees.file_name()? != "worktrees" {
        return None;
    }
    let common = worktrees.parent()?;
    let candidate = common.parent()?;

    // Verify the inference instead of trusting the path shape: the
    // candidate's own `.git` must BE this common git dir.
    let common_key = std::fs::canonicalize(common).ok()?;
    let candidate_key = std::fs::canonicalize(candidate.join(".git")).ok()?;
    if common_key != candidate_key {
        return None;
    }
    Some(candidate.to_path_buf())
}

/// project_filter のマッチング:
/// Task.project が filter と完全一致、または filter で始まる場合にマッチ。
fn project_matches(task_project: &str, filter: &str) -> bool {
    if task_project == filter {
        return true;
    }
    // filter が末尾スラッシュなしの repo_root の場合を考慮
    // task_project が filter + "/" で始まればマッチ
    if let Some(rest) = task_project.strip_prefix(filter) {
        if rest.starts_with('/') {
            return true;
        }
    }
    bare_name_matches_path(task_project, filter) || bare_name_matches_path(filter, task_project)
}

/// True iff `bare` has no path separator (a short project label, e.g.
/// `"aegis"`) and equals the final path component of `path_like`. Bridges
/// historical project-key drift (backlog id b92a7a77) where the SAME project
/// was recorded once as a bare short name and once as its absolute repo path
/// — before [`add_with_weight`] started canonicalizing new writes — without
/// rewriting either already-stored value.
fn bare_name_matches_path(bare: &str, path_like: &str) -> bool {
    if bare.is_empty() || bare.contains('/') {
        return false;
    }
    Path::new(path_like).file_name().is_some_and(|f| f == bare)
}

/// 現在の Unix タイムスタンプ (秒)。
fn now_unix() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// ---- tests ------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Mutex;

    /// Serializes every test in this module that either mutates the
    /// process-global `PATH` or spawns a real `git` subprocess. `PATH` is
    /// process-wide, so a PATH-mutating test running concurrently with a
    /// real-git test (cargo test's default thread-parallel execution) can
    /// transiently blank the other thread's `PATH` mid-spawn, failing it with
    /// a spurious `Os { code: 2, NotFound }`. Mirrors donegate's
    /// `PROBE_PATH_ENV_LOCK` (crates/donegate/src/git.rs).
    static PROBE_PATH_ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Deterministic proof that `PROBE_PATH_ENV_LOCK` actually serializes
    /// holders (independent of the flaky, timing-dependent real-git/PATH
    /// race it protects against). 8 threads race to increment a shared
    /// counter, hold it briefly, then decrement; the max observed
    /// concurrent-holder count must be exactly 1 if the lock is taken.
    #[test]
    fn probe_path_env_lock_actually_serializes_concurrent_holders() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        use std::thread;
        use std::time::Duration;

        let concurrent_holders = Arc::new(AtomicUsize::new(0));
        let max_observed = Arc::new(AtomicUsize::new(0));

        let handles: Vec<_> = (0..8)
            .map(|_| {
                let concurrent_holders = Arc::clone(&concurrent_holders);
                let max_observed = Arc::clone(&max_observed);
                thread::spawn(move || {
                    let _g = PROBE_PATH_ENV_LOCK
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    let now = concurrent_holders.fetch_add(1, Ordering::SeqCst) + 1;
                    max_observed.fetch_max(now, Ordering::SeqCst);
                    thread::sleep(Duration::from_millis(20));
                    concurrent_holders.fetch_sub(1, Ordering::SeqCst);
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(
            max_observed.load(Ordering::SeqCst),
            1,
            "PROBE_PATH_ENV_LOCK must serialize holders; observed {} concurrent holders",
            max_observed.load(Ordering::SeqCst)
        );
    }

    fn tmp_path() -> PathBuf {
        let dir = tempfile::tempdir().expect("tmp dir");
        // keep the dir alive by leaking — acceptable in tests
        let path = dir.path().join("tasks.toml");
        std::mem::forget(dir);
        path
    }

    #[test]
    fn load_missing_file_returns_empty() {
        let path = PathBuf::from("/nonexistent/tasks.toml");
        let tasks = load(&path).unwrap();
        assert!(tasks.is_empty());
    }

    #[test]
    fn add_and_load_roundtrip() {
        let path = tmp_path();
        let id = add(
            &path,
            "Test task",
            "/repo",
            vec!["p1".into()],
            "notes",
            1000,
        )
        .unwrap();
        assert_eq!(id.len(), 8);
        let tasks = load(&path).unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, id);
        assert_eq!(tasks[0].title, "Test task");
        assert_eq!(tasks[0].status, "pending");
    }

    #[test]
    fn next_returns_highest_priority() {
        let path = tmp_path();
        add(&path, "Low", "/repo", vec!["p2".into()], "", 100).unwrap();
        add(&path, "High", "/repo", vec!["p0".into()], "", 200).unwrap();
        add(&path, "Mid", "/repo", vec!["p1".into()], "", 150).unwrap();
        let t = next(&path, None, None).unwrap().unwrap();
        assert_eq!(t.title, "High");
    }

    #[test]
    fn next_same_priority_by_created_at() {
        let path = tmp_path();
        add(&path, "B", "/repo", vec!["p1".into()], "", 200).unwrap();
        add(&path, "A", "/repo", vec!["p1".into()], "", 100).unwrap();
        let t = next(&path, None, None).unwrap().unwrap();
        assert_eq!(t.title, "A");
    }

    #[test]
    fn next_skips_done_tasks() {
        let path = tmp_path();
        let id = add(&path, "Done task", "/repo", vec![], "", 100).unwrap();
        mark_done(&path, &id).unwrap();
        let result = next(&path, None, None).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn mark_done_updates_status() {
        let path = tmp_path();
        let id = add(&path, "Task", "/repo", vec![], "", 100).unwrap();
        mark_done(&path, &id).unwrap();
        let tasks = load(&path).unwrap();
        assert_eq!(tasks[0].status, "done");
    }

    #[test]
    fn mark_done_unknown_id_errors() {
        let path = tmp_path();
        add(&path, "Task", "/repo", vec![], "", 100).unwrap();
        assert!(mark_done(&path, "nonexistent").is_err());
    }

    /// CA-backlog-003: calling `mark_done` twice for the SAME id (e.g. a
    /// retried call after a caller-side timeout, or `/flow` re-driving a step
    /// after a partial failure) must be idempotent — the second call must
    /// succeed as a no-op, not error, and must not re-stamp `updated_at`.
    #[test]
    fn mark_done_is_idempotent_on_repeat_call() {
        let path = tmp_path();
        let id = add(&path, "Task", "/repo", vec![], "", 100).unwrap();
        mark_done(&path, &id).unwrap();
        let updated_at_first = load(&path).unwrap()[0].updated_at;

        // Second call for the same already-done id must succeed (not error)
        // and must be a true no-op.
        let result = mark_done(&path, &id);
        assert!(
            result.is_ok(),
            "repeat mark_done on an already-done task must not error"
        );
        let tasks = load(&path).unwrap();
        assert_eq!(tasks[0].status, "done");
        assert_eq!(
            tasks[0].updated_at, updated_at_first,
            "repeat mark_done must not re-stamp updated_at (true no-op)"
        );
    }

    #[test]
    fn mark_failed_appends_reason() {
        let path = tmp_path();
        let id = add(&path, "Task", "/repo", vec![], "existing note", 100).unwrap();
        mark_failed(&path, &id, Some("timeout")).unwrap();
        let tasks = load(&path).unwrap();
        assert_eq!(tasks[0].status, "failed");
        assert!(tasks[0].notes.contains("timeout"));
        assert!(tasks[0].notes.contains("existing note"));
    }

    /// CA-backlog-003: calling `mark_failed` twice for the SAME id must be
    /// idempotent — the second call must succeed as a no-op, must NOT
    /// re-append `reason` to `notes` a second time, and must NOT push
    /// `defer_until` further into the future again.
    #[test]
    fn mark_failed_is_idempotent_on_repeat_call() {
        let path = tmp_path();
        let id = add(&path, "Task", "/repo", vec![], "", 100).unwrap();
        mark_failed(&path, &id, Some("timeout")).unwrap();
        let after_first = load(&path).unwrap();
        let notes_first = after_first[0].notes.clone();
        let defer_first = after_first[0].defer_until;
        let updated_at_first = after_first[0].updated_at;

        let result = mark_failed(&path, &id, Some("timeout"));
        assert!(
            result.is_ok(),
            "repeat mark_failed on an already-failed task must not error"
        );
        let tasks = load(&path).unwrap();
        assert_eq!(tasks[0].status, "failed");
        assert_eq!(
            tasks[0].notes, notes_first,
            "repeat mark_failed must not re-append reason (true no-op)"
        );
        assert_eq!(
            tasks[0].defer_until, defer_first,
            "repeat mark_failed must not push defer_until further out"
        );
        assert_eq!(
            tasks[0].updated_at, updated_at_first,
            "repeat mark_failed must not re-stamp updated_at (true no-op)"
        );
    }

    #[test]
    fn edit_updates_fields() {
        let path = tmp_path();
        let id = add(&path, "Old title", "/repo", vec![], "", 100).unwrap();
        edit(&path, &id, Some("New title"), None, Some("new notes"), None).unwrap();
        let tasks = load(&path).unwrap();
        assert_eq!(tasks[0].title, "New title");
        assert_eq!(tasks[0].notes, "new notes");
        assert_eq!(tasks[0].tags.len(), 0); // unchanged
    }

    #[test]
    fn list_with_status_filter() {
        let path = tmp_path();
        let id = add(&path, "Task A", "/repo", vec![], "", 100).unwrap();
        add(&path, "Task B", "/repo", vec![], "", 200).unwrap();
        mark_done(&path, &id).unwrap();
        let pending = list(&path, None, None, Some("pending")).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].title, "Task B");
    }

    #[test]
    fn list_with_tag_filter() {
        let path = tmp_path();
        add(&path, "Tagged", "/repo", vec!["bug".into()], "", 100).unwrap();
        add(&path, "Untagged", "/repo", vec![], "", 200).unwrap();
        let result = list(&path, Some("bug"), None, None).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].title, "Tagged");
    }

    #[test]
    fn project_filter_exact_match() {
        assert!(project_matches("/repo/foo", "/repo/foo"));
    }

    #[test]
    fn project_filter_prefix_with_slash() {
        assert!(project_matches("/repo/foo/bar", "/repo/foo"));
    }

    #[test]
    fn project_filter_no_match() {
        assert!(!project_matches("/repo/foobar", "/repo/foo"));
    }

    /// CA-backlog-007 (verified bypass): `canonicalize_project` passes a bare
    /// (non-path-shaped) `--project` label through unchanged, so it remains
    /// freely writable today — not just a historical artifact. Combined with
    /// `bare_name_matches_path` bridging ANY path sharing that basename (with
    /// no registry binding the label to one specific project), a bare label
    /// that collides with an unrelated, already-known, differently-pathed
    /// project risks leaking one project's tasks into the other's filtered
    /// view. Blocking the write once that collision is knowable closes the
    /// gap going forward (fail closed on the ambiguity, per this repo's own
    /// doctrine) without touching the read-side historical-drift bridge.
    #[test]
    fn add_rejects_a_bare_project_label_that_collides_with_a_different_known_path() {
        let path = tmp_path();
        add(&path, "Existing", "/home/bob/work/aegis", vec![], "", 100).unwrap();
        let err = add(&path, "New", "aegis", vec![], "", 200).unwrap_err();
        assert!(
            err.to_string().contains("ambiguous"),
            "expected an ambiguity error, got: {err}"
        );
        let tasks = load(&path).unwrap();
        assert_eq!(tasks.len(), 1, "the rejected add must not persist a task");
    }

    /// Reusing the SAME bare label repeatedly (no other project shares its
    /// basename) must keep working — this guard targets collisions, not bare
    /// labels in general.
    #[test]
    fn add_allows_reusing_the_same_bare_project_label_repeatedly() {
        let path = tmp_path();
        add(&path, "First", "aegis", vec![], "", 100).unwrap();
        add(&path, "Second", "aegis", vec![], "", 200).unwrap();
        let tasks = load(&path).unwrap();
        assert_eq!(tasks.len(), 2);
    }

    #[test]
    fn project_filter_bridges_bare_name_and_absolute_path() {
        // backlog id b92a7a77: historical drift where the SAME project was
        // recorded once as a bare short label and once as its absolute path.
        assert!(project_matches("aegis", "/mnt/c/Users/x/src/aegis"));
        assert!(project_matches("/mnt/c/Users/x/src/aegis", "aegis"));
    }

    #[test]
    fn project_filter_bare_name_does_not_match_unrelated_path() {
        assert!(!project_matches("harness", "/mnt/c/Users/x/src/aegis"));
    }

    #[test]
    fn project_filter_bare_name_vs_bare_name_only_matches_exact() {
        // Neither side is path-shaped, so only the pre-existing exact-match
        // branch applies — no accidental cross-match between distinct bare
        // labels.
        assert!(!project_matches("aegis", "harness"));
    }

    #[test]
    fn canonicalize_project_leaves_bare_name_unchanged() {
        assert_eq!(canonicalize_project("aegis"), "aegis");
    }

    #[test]
    fn canonicalize_project_resolves_subdir_to_repo_root() {
        let _g = PROBE_PATH_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        // A path-shaped project value resolves through git-toplevel rather
        // than being stored verbatim, so `--project "$PWD"` from any subdir
        // of a repo lands on the same string.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        assert!(std::process::Command::new("git")
            .arg("-C")
            .arg(&root)
            .arg("init")
            .arg("-q")
            .status()
            .unwrap()
            .success());
        let sub = root.join("a/b");
        std::fs::create_dir_all(&sub).unwrap();
        assert_eq!(
            canonicalize_project(root.to_str().unwrap()),
            canonicalize_project(sub.to_str().unwrap()),
        );
    }

    #[test]
    fn add_canonicalizes_project_before_storing() {
        let _g = PROBE_PATH_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let path = tmp_path();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        assert!(std::process::Command::new("git")
            .arg("-C")
            .arg(&root)
            .arg("init")
            .arg("-q")
            .status()
            .unwrap()
            .success());
        let sub = root.join("nested");
        std::fs::create_dir_all(&sub).unwrap();
        let id = add(&path, "Task", sub.to_str().unwrap(), vec![], "", 100).unwrap();
        let tasks = load(&path).unwrap();
        let stored = &tasks.iter().find(|t| t.id == id).unwrap().project;
        assert_eq!(stored, &canonicalize_project(root.to_str().unwrap()));
    }

    /// CA-backlog-006 (verified bypass): `add_with_weight` canonicalizes a
    /// path-shaped project through `canonicalize_project` (git toplevel +
    /// `canonicalize()`, which resolves symlinks) before storing it. Read-side
    /// filters (CLI `--project`, and SessionStart's `repo_root()`) passed the
    /// raw path straight into `project_matches` with no equivalent
    /// resolution, so a task looked up via a symlinked path to the SAME repo
    /// silently failed to match its own canonical stored project.
    #[test]
    fn list_matches_a_project_filter_reached_via_a_symlinked_path() {
        let _g = PROBE_PATH_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let path = tmp_path();
        let real_dir = tempfile::tempdir().unwrap();
        let real_root = real_dir.path().canonicalize().unwrap();
        assert!(std::process::Command::new("git")
            .arg("-C")
            .arg(&real_root)
            .arg("init")
            .arg("-q")
            .status()
            .unwrap()
            .success());

        let link_parent = tempfile::tempdir().unwrap();
        let link_path = link_parent.path().join("via-symlink");
        std::os::unix::fs::symlink(&real_root, &link_path).unwrap();

        // Stored via the REAL (post-canonicalization) path, mirroring what
        // `add_with_weight` actually persists.
        add(&path, "Task", real_root.to_str().unwrap(), vec![], "", 100).unwrap();

        // Looked up via the SYMLINK path — a different string, same repo.
        let tasks = list(&path, None, Some(link_path.to_str().unwrap()), None).unwrap();
        assert_eq!(
            tasks.len(),
            1,
            "a task must be found via a symlinked path to the same repo"
        );
    }

    #[test]
    fn next_with_project_filter() {
        let path = tmp_path();
        add(&path, "In repo", "/repo/proj", vec![], "", 100).unwrap();
        add(&path, "Other", "/other/proj", vec![], "", 100).unwrap();
        let t = next(&path, None, Some("/repo/proj")).unwrap().unwrap();
        assert_eq!(t.title, "In repo");
    }

    #[test]
    fn mark_failed_sets_defer_until() {
        let path = tmp_path();
        let id = add(&path, "Task", "/repo", vec![], "", 100).unwrap();
        mark_failed(&path, &id, Some("error")).unwrap();
        let tasks = load(&path).unwrap();
        assert_eq!(tasks[0].status, "failed");
        // defer_until は Some で、now より未来であること
        let defer = tasks[0].defer_until.expect("defer_until should be set");
        // 172800 秒 (2日) 後を設定しているため now + 172800 付近であること
        assert!(defer > now_unix());
    }

    #[test]
    fn next_skips_deferred_task() {
        let path = tmp_path();
        let id = add(&path, "Will fail", "/repo", vec![], "", 1000).unwrap();
        // mark_failed でタスクが defer される
        mark_failed(&path, &id, None).unwrap();
        // deferred なので next は None を返す
        let result = next(&path, None, None).unwrap();
        assert!(
            result.is_none(),
            "deferred task should not be returned by next"
        );
    }

    #[test]
    fn requeue_expired_restores_pending() {
        let path = tmp_path();
        let id = add(&path, "Task", "/repo", vec![], "", 1000).unwrap();
        mark_failed(&path, &id, None).unwrap();

        // defer 直後は next がスキップ
        assert!(next(&path, None, None).unwrap().is_none());

        // 期限を過去に設定するため、直接 load → edit → save する
        let mut tasks = load(&path).unwrap();
        tasks[0].defer_until = Some(500); // 過去のタイムスタンプ
        save(&path, &tasks).unwrap();

        // requeue_expired(now=1000) で期限切れタスクが復帰
        let count = requeue_expired(&path, 1000).unwrap();
        assert_eq!(count, 1);

        let tasks = load(&path).unwrap();
        assert_eq!(tasks[0].status, "pending");
        assert!(tasks[0].defer_until.is_none());

        // next でも取得できるようになる
        let t = next(&path, None, None).unwrap();
        assert!(t.is_some());
    }

    /// F→P regression oracle for the TOCTOU lost-update race in
    /// `requeue_expired`. Many threads concurrently requeue the same expired
    /// tasks while one thread does an independent `add_with_weight` on the SAME
    /// file. On the unprotected load-modify-save, one side's write clobbers the
    /// other (a requeue drops the newly-added task, or the add re-persists the
    /// stale still-deferred state) — so this is reliably RED before the scoped
    /// lock and GREEN after. Repeated over several iterations to force the race.
    #[test]
    fn requeue_expired_no_lost_update_under_concurrency() {
        use std::sync::{Arc, Barrier};

        for iter in 0..20 {
            let path = tmp_path();

            // Seed several expired, non-pending tasks (defer_until in the past
            // relative to now=1000, status=failed).
            let mut seed = Vec::new();
            for i in 0..6 {
                seed.push(Task {
                    id: format!("exp{i}"),
                    title: format!("expired-{i}"),
                    project: "/repo".to_string(),
                    project_unresolved: false,
                    tags: vec![],
                    status: STATUS_FAILED.to_string(),
                    notes: String::new(),
                    created_at: 100,
                    updated_at: 100,
                    defer_until: Some(500),
                    weight: 0.0,
                    issue_number: None,
                    issue_url: None,
                });
            }
            save(&path, &seed).unwrap();

            const N: usize = 12;
            // N requeue threads + 1 concurrent-add thread all rendezvous here.
            let barrier = Arc::new(Barrier::new(N + 1));
            let path = Arc::new(path);
            let added_title = format!("concurrent-add-{iter}");

            let mut handles = Vec::with_capacity(N + 1);
            for _ in 0..N {
                let path = Arc::clone(&path);
                let barrier = Arc::clone(&barrier);
                handles.push(std::thread::spawn(move || {
                    barrier.wait();
                    // Must never error (fail-soft) even under contention.
                    requeue_expired(path.as_path(), 1000).expect("requeue must not error");
                }));
            }
            {
                let path = Arc::clone(&path);
                let barrier = Arc::clone(&barrier);
                let added_title = added_title.clone();
                handles.push(std::thread::spawn(move || {
                    barrier.wait();
                    add_with_weight(
                        path.as_path(),
                        &added_title,
                        "/repo",
                        vec![],
                        "",
                        0.0,
                        false,
                        2000,
                    )
                    .expect("add must not error");
                }));
            }
            for h in handles {
                h.join().expect("thread join");
            }

            let final_tasks = load(path.as_path()).unwrap();

            // 1. No expired task lost its requeue: all 6 present AND pending.
            let expired: Vec<&Task> = final_tasks
                .iter()
                .filter(|t| t.id.starts_with("exp"))
                .collect();
            assert_eq!(
                expired.len(),
                6,
                "iter {iter}: expired tasks were dropped (lost update)"
            );
            for t in &expired {
                assert_eq!(
                    t.status, "pending",
                    "iter {iter}: expired task {} lost its requeue",
                    t.id
                );
                assert!(
                    t.defer_until.is_none(),
                    "iter {iter}: expired task {} still deferred",
                    t.id
                );
            }

            // 2. The concurrently-added task survived (was not clobbered).
            assert!(
                final_tasks.iter().any(|t| t.title == added_title),
                "iter {iter}: concurrently-added task was lost (lost-update race)"
            );
        }
    }

    #[test]
    fn requeue_expired_returns_zero_when_none_expired() {
        let path = tmp_path();
        let id = add(&path, "Task", "/repo", vec![], "", 1000).unwrap();
        mark_failed(&path, &id, None).unwrap();
        // now を小さい値にして期限切れタスクがない状態でテスト
        let count = requeue_expired(&path, 0).unwrap();
        assert_eq!(count, 0);
    }

    // --- weight ordering (opportunity weight drives the source-layer queue) ---

    #[test]
    fn next_orders_by_weight_desc_within_priority() {
        let path = tmp_path();
        // Same priority (p1). Insertion/created_at order would put "First" ahead,
        // but the higher weight must win.
        add_with_weight(
            &path,
            "Light",
            "/repo",
            vec!["p1".into()],
            "",
            0.2,
            false,
            100,
        )
        .unwrap();
        add_with_weight(
            &path,
            "Heavy",
            "/repo",
            vec!["p1".into()],
            "",
            0.9,
            false,
            200,
        )
        .unwrap();
        add_with_weight(
            &path,
            "Mid",
            "/repo",
            vec!["p1".into()],
            "",
            0.5,
            false,
            150,
        )
        .unwrap();
        let t = next(&path, None, None).unwrap().unwrap();
        assert_eq!(
            t.title, "Heavy",
            "highest weight wins within the priority tier"
        );
    }

    #[test]
    fn priority_dominates_weight() {
        let path = tmp_path();
        // A heavy p2 must still sit behind a light p0: priority is the primary key.
        add_with_weight(
            &path,
            "Heavy p2",
            "/repo",
            vec!["p2".into()],
            "",
            9.0,
            false,
            100,
        )
        .unwrap();
        add_with_weight(
            &path,
            "Light p0",
            "/repo",
            vec!["p0".into()],
            "",
            0.1,
            false,
            200,
        )
        .unwrap();
        let t = next(&path, None, None).unwrap().unwrap();
        assert_eq!(t.title, "Light p0");
    }

    #[test]
    fn equal_weight_falls_back_to_created_at() {
        let path = tmp_path();
        // Equal weight → the legacy FIFO (created_at asc) tie-break still applies.
        add_with_weight(
            &path,
            "Newer",
            "/repo",
            vec!["p1".into()],
            "",
            0.5,
            false,
            200,
        )
        .unwrap();
        add_with_weight(
            &path,
            "Older",
            "/repo",
            vec!["p1".into()],
            "",
            0.5,
            false,
            100,
        )
        .unwrap();
        let t = next(&path, None, None).unwrap().unwrap();
        assert_eq!(t.title, "Older");
    }

    #[test]
    fn changing_weight_changes_next_pick() {
        // The load-bearing assertion: editing weight reorders the queue.
        let path = tmp_path();
        add_with_weight(&path, "A", "/repo", vec!["p1".into()], "", 0.3, false, 100).unwrap();
        add_with_weight(&path, "B", "/repo", vec!["p1".into()], "", 0.6, false, 200).unwrap();
        // Initially B (heavier) is next.
        assert_eq!(next(&path, None, None).unwrap().unwrap().title, "B");

        // Bump A above B and persist.
        let mut tasks = load(&path).unwrap();
        for t in tasks.iter_mut() {
            if t.title == "A" {
                t.weight = 0.9;
            }
        }
        save(&path, &tasks).unwrap();

        // Now A is next — the same store, only the weight changed the order.
        assert_eq!(next(&path, None, None).unwrap().unwrap().title, "A");
    }

    #[test]
    fn list_is_weight_ordered() {
        let path = tmp_path();
        add_with_weight(
            &path,
            "Light",
            "/repo",
            vec!["p1".into()],
            "",
            0.2,
            false,
            100,
        )
        .unwrap();
        add_with_weight(
            &path,
            "Heavy",
            "/repo",
            vec!["p1".into()],
            "",
            0.9,
            false,
            200,
        )
        .unwrap();
        let titles: Vec<String> = list(&path, None, None, Some("pending"))
            .unwrap()
            .into_iter()
            .map(|t| t.title)
            .collect();
        assert_eq!(titles, vec!["Heavy".to_string(), "Light".to_string()]);
    }

    // --- duplicate content guard (hashkey dedup on add) ---
    //
    // NB: `is_claimed_elsewhere` shells out to `condukt state is-claimed`. In
    // this dev environment the installed `condukt` binary is a stale build
    // that lacks the `is-claimed` subcommand and exits non-zero/non-one
    // (clap "unrecognized subcommand" -> exit 2), which the fail-soft path
    // correctly treats as "not claimed". These tests exercise the (a) local
    // pending/failed guard, independent of whatever `condukt` happens to be
    // on PATH.

    #[test]
    fn add_rejects_duplicate_pending_hashkey() {
        let path = tmp_path();
        add(&path, "Fix login", "/repo", vec![], "", 100).unwrap();
        // Same content (trivially reworded) while the first is still pending.
        let err = add_with_weight(
            &path,
            "  fix   LOGIN! ",
            "/repo",
            vec![],
            "",
            0.0,
            false,
            200,
        )
        .expect_err("duplicate pending task must be rejected");
        assert!(err.to_string().contains("duplicate"), "got: {err}");
        // Only the first task was persisted.
        assert_eq!(load(&path).unwrap().len(), 1);
    }

    #[test]
    fn add_rejects_duplicate_failed_hashkey() {
        let path = tmp_path();
        let id = add(&path, "Fix login", "/repo", vec![], "", 100).unwrap();
        mark_failed(&path, &id, Some("timeout")).unwrap();
        let err = add(&path, "Fix login", "/repo", vec![], "", 200)
            .expect_err("duplicate failed task must be rejected");
        assert!(err.to_string().contains("duplicate"), "got: {err}");
        assert_eq!(load(&path).unwrap().len(), 1);
    }

    #[test]
    fn add_force_bypasses_duplicate_guard() {
        let path = tmp_path();
        add(&path, "Fix login", "/repo", vec![], "", 100).unwrap();
        let id = add_with_weight(&path, "Fix login", "/repo", vec![], "", 0.0, true, 200)
            .expect("force must bypass the duplicate guard");
        let tasks = load(&path).unwrap();
        assert_eq!(tasks.len(), 2);
        assert!(tasks.iter().any(|t| t.id == id));
    }

    #[test]
    fn add_does_not_block_on_done_duplicate() {
        let path = tmp_path();
        let id = add(&path, "Fix login", "/repo", vec![], "", 100).unwrap();
        mark_done(&path, &id).unwrap();
        // A done task with the same title/project must NOT block a new add.
        let new_id = add(&path, "Fix login", "/repo", vec![], "", 200)
            .expect("a done duplicate must not block a new add");
        let tasks = load(&path).unwrap();
        assert_eq!(tasks.len(), 2);
        assert!(tasks.iter().any(|t| t.id == new_id));
    }

    // --- CA-backlog-001: next() is a pure read; next_claim() is atomic -----

    /// F→P regression oracle for CA-backlog-001. `next()` is documented as a
    /// pure read (no lock, no mutation): confirms two concurrent plain `next()`
    /// calls against a SINGLE pending task both return that SAME task (proving
    /// the vulnerability the finding describes still exists for the default,
    /// backward-compatible path — `next` itself does not change).
    #[test]
    fn next_plain_is_unsynchronized_pure_read() {
        let path = tmp_path();
        let id = add(&path, "Only task", "/repo", vec![], "", 100).unwrap();

        let a = next(&path, None, None).unwrap().unwrap();
        let b = next(&path, None, None).unwrap().unwrap();
        // Both reads see the same still-pending task — this is the documented
        // (pre-existing, unchanged) behavior of the default `next`.
        assert_eq!(a.id, id);
        assert_eq!(b.id, id);
        assert_eq!(a.status, "pending");
        assert_eq!(b.status, "pending");
    }

    /// Concurrency budget for [`claim_one_with_retry`]. A `None` from
    /// `next_claim` is ambiguous — it means EITHER "the queue is empty" OR
    /// "I declined because I could not take the tasks-file lock this instant"
    /// (the documented fail-closed decline). A test that treats the first
    /// `None` as "empty" would therefore fail intermittently under contention
    /// while proving nothing, so retry a bounded number of times.
    const CLAIM_RETRIES: usize = 500;

    fn claim_one_with_retry(path: &Path, project: Option<&str>) -> Option<Task> {
        for _ in 0..CLAIM_RETRIES {
            if let Ok(Some(t)) = next_claim(path, None, project) {
                return Some(t);
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        None
    }

    /// The monopoly this whole change is about: several drivers pulling from
    /// the SAME project's queue at the same time must each be handed a
    /// DIFFERENT task, and none of them may be refused. This is what makes the
    /// exclusive project-wide `backlog lock` unnecessary for exclusivity —
    /// `next_claim` already provides it at task granularity, so `/flow` no
    /// longer has to serialize whole sessions to avoid double-dispatch.
    ///
    /// RED against the plain `next()` (see
    /// `concurrent_plain_next_hands_every_driver_the_same_task`, which pins
    /// that vulnerability): there, every driver receives the identical task.
    #[test]
    fn concurrent_drivers_claim_disjoint_tasks_and_none_is_refused() {
        use std::sync::{Arc, Barrier, Mutex};

        for iter in 0..10 {
            let path = tmp_path();
            // More tasks than drivers, so "refused" can only mean contention,
            // never an empty queue.
            let mut ids = Vec::new();
            for i in 0..8 {
                ids.push(
                    add(
                        &path,
                        &format!("Task {i}"),
                        "/repo",
                        vec![],
                        "",
                        100 + i as i64,
                    )
                    .unwrap(),
                );
            }

            const DRIVERS: usize = 4;
            let barrier = Arc::new(Barrier::new(DRIVERS));
            let path = Arc::new(path);
            let got: Arc<Mutex<Vec<Option<String>>>> = Arc::new(Mutex::new(vec![None; DRIVERS]));

            let mut handles = Vec::with_capacity(DRIVERS);
            for d in 0..DRIVERS {
                let path = Arc::clone(&path);
                let barrier = Arc::clone(&barrier);
                let got = Arc::clone(&got);
                handles.push(std::thread::spawn(move || {
                    barrier.wait();
                    let claimed = claim_one_with_retry(path.as_path(), Some("/repo"));
                    got.lock().expect("lock")[d] = claimed.map(|t| t.id);
                }));
            }
            for h in handles {
                h.join().expect("thread join");
            }

            let got = got.lock().expect("lock").clone();
            // (a) Nobody stood down: every concurrent driver got work.
            for (d, g) in got.iter().enumerate() {
                assert!(
                    g.is_some(),
                    "iter {iter}: driver {d} was refused work while {} pending tasks existed",
                    ids.len()
                );
            }
            // (b) Disjoint: no two drivers were handed the same task.
            let mut seen: Vec<&String> = got.iter().flatten().collect();
            seen.sort();
            let before = seen.len();
            seen.dedup();
            assert_eq!(
                before,
                seen.len(),
                "iter {iter}: two concurrent drivers were handed the same task: {got:?}"
            );
            // (c) Persisted state agrees: exactly DRIVERS tasks are `claimed`.
            let tasks = load(path.as_path()).unwrap();
            let claimed: Vec<&Task> = tasks
                .iter()
                .filter(|t| t.status == STATUS_CLAIMED)
                .collect();
            assert_eq!(
                claimed.len(),
                DRIVERS,
                "iter {iter}: exactly one claim per driver must be persisted"
            );
        }
    }

    /// The vulnerability the above fixes, pinned so it cannot be mistaken for
    /// a safe alternative: with the plain (documented pure-read) `next()`,
    /// concurrent drivers all receive the SAME task. This is why `/flow` must
    /// pick with `next --claim` now that it no longer holds an exclusive lock.
    #[test]
    fn concurrent_plain_next_hands_every_driver_the_same_task() {
        let path = tmp_path();
        for i in 0..8 {
            add(
                &path,
                &format!("Task {i}"),
                "/repo",
                vec![],
                "",
                100 + i as i64,
            )
            .unwrap();
        }
        let a = next(&path, None, Some("/repo")).unwrap().unwrap();
        let b = next(&path, None, Some("/repo")).unwrap().unwrap();
        assert_eq!(
            a.id, b.id,
            "plain next() is a pure read: two drivers get the identical task"
        );
    }

    /// F→P regression oracle for CA-backlog-001 (the fix). Many threads race
    /// `next_claim` against a SINGLE pending task concurrently. Atomicity
    /// (claim happens inside the same tasks-file-lock critical section as
    /// selection) must guarantee EXACTLY ONE of them observes the task as the
    /// winner — never zero, never more than one. This is reliably RED
    /// (multiple winners) against the old unsynchronized `next()`, and GREEN
    /// against `next_claim()`.
    #[test]
    fn next_claim_concurrent_callers_never_double_claim() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::{Arc, Barrier};

        for iter in 0..15 {
            let path = tmp_path();
            let id = add(&path, "Contended task", "/repo", vec![], "", 100).unwrap();

            const N: usize = 16;
            let barrier = Arc::new(Barrier::new(N));
            let path = Arc::new(path);
            let winners = Arc::new(AtomicUsize::new(0));

            let mut handles = Vec::with_capacity(N);
            for _ in 0..N {
                let path = Arc::clone(&path);
                let barrier = Arc::clone(&barrier);
                let winners = Arc::clone(&winners);
                let id = id.clone();
                handles.push(std::thread::spawn(move || {
                    barrier.wait();
                    if let Ok(Some(t)) = next_claim(path.as_path(), None, None) {
                        if t.id == id {
                            winners.fetch_add(1, Ordering::SeqCst);
                        }
                    }
                }));
            }
            for h in handles {
                h.join().expect("thread join");
            }

            assert_eq!(
                winners.load(Ordering::SeqCst),
                1,
                "iter {iter}: exactly one concurrent next_claim caller must win the single \
                 pending task (CA-backlog-001); saw {} winners",
                winners.load(Ordering::SeqCst)
            );

            // The task itself must now be persisted as claimed exactly once.
            let tasks = load(path.as_path()).unwrap();
            let claimed_count = tasks
                .iter()
                .filter(|t| t.id == id && t.status == "claimed")
                .count();
            assert_eq!(claimed_count, 1, "iter {iter}: task must end up claimed");
        }
    }

    /// CA-backlog-003: stress test for the stale-lock reap-window race. Many
    /// threads concurrently `add_with_weight` NEW tasks onto the SAME file
    /// while many other threads concurrently `next_claim` from it, all
    /// rendezvous'd at a barrier to maximize lock contention (so waiters queue
    /// up behind whichever thread currently holds `with_tasks_lock`). If the
    /// blocking-acquire retry budget is too small relative to how long a
    /// legitimate holder can keep the lock (e.g. `add`'s `is_claimed_elsewhere`
    /// subprocess bound), a waiting racer gives up and falls back to
    /// unprotected fail-soft execution, or a stale-but-still-live lockfile
    /// gets reaped mid-critical-section — either way causing a lost update
    /// (dropped add, or two `next_claim` callers winning the same task).
    /// Asserts: (a) every add is durably persisted (no lost update), (b) no
    /// two `next_claim` winners ever claim the same task id, and (c) nothing
    /// errors under contention (fail-soft, never breaks a caller).
    #[test]
    fn add_and_claim_no_lost_update_under_heavy_contention() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::{Arc, Barrier, Mutex};

        for iter in 0..8 {
            let path = tmp_path();

            // Seed several pending tasks for the claimers to race over.
            const SEEDED: usize = 8;
            let mut seed = Vec::new();
            for i in 0..SEEDED {
                seed.push(Task {
                    id: format!("seed{iter}-{i}"),
                    title: format!("seeded-{iter}-{i}"),
                    project: "/repo".to_string(),
                    project_unresolved: false,
                    tags: vec![],
                    status: STATUS_PENDING.to_string(),
                    notes: String::new(),
                    created_at: 100,
                    updated_at: 100,
                    defer_until: None,
                    weight: 0.0,
                    issue_number: None,
                    issue_url: None,
                });
            }
            save(&path, &seed).unwrap();

            const ADDERS: usize = 10;
            const CLAIMERS: usize = 10;
            let barrier = Arc::new(Barrier::new(ADDERS + CLAIMERS));
            let path = Arc::new(path);
            let winners: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
            let claim_errors = Arc::new(AtomicUsize::new(0));
            let add_errors = Arc::new(AtomicUsize::new(0));

            let mut handles = Vec::with_capacity(ADDERS + CLAIMERS);

            for i in 0..ADDERS {
                let path = Arc::clone(&path);
                let barrier = Arc::clone(&barrier);
                let add_errors = Arc::clone(&add_errors);
                handles.push(std::thread::spawn(move || {
                    barrier.wait();
                    let title = format!("concurrent-add-{iter}-{i}");
                    if add_with_weight(
                        path.as_path(),
                        &title,
                        "/repo",
                        vec![],
                        "",
                        0.0,
                        false,
                        2000 + i as i64,
                    )
                    .is_err()
                    {
                        add_errors.fetch_add(1, Ordering::SeqCst);
                    }
                }));
            }

            for _ in 0..CLAIMERS {
                let path = Arc::clone(&path);
                let barrier = Arc::clone(&barrier);
                let winners = Arc::clone(&winners);
                let claim_errors = Arc::clone(&claim_errors);
                handles.push(std::thread::spawn(move || {
                    barrier.wait();
                    match next_claim(path.as_path(), None, None) {
                        Ok(Some(t)) => winners.lock().unwrap().push(t.id),
                        Ok(None) => {}
                        Err(_) => {
                            claim_errors.fetch_add(1, Ordering::SeqCst);
                        }
                    }
                }));
            }

            for h in handles {
                h.join().expect("thread join");
            }

            assert_eq!(
                add_errors.load(Ordering::SeqCst),
                0,
                "iter {iter}: add_with_weight must never error under contention (fail-soft)"
            );
            assert_eq!(
                claim_errors.load(Ordering::SeqCst),
                0,
                "iter {iter}: next_claim must never error under contention (fail-soft)"
            );

            let final_tasks = load(path.as_path()).unwrap();

            // (a) No lost update: all ADDERS concurrently-added tasks survived.
            for i in 0..ADDERS {
                let title = format!("concurrent-add-{iter}-{i}");
                assert!(
                    final_tasks.iter().any(|t| t.title == title),
                    "iter {iter}: concurrently-added task {title} was lost (lost-update race)"
                );
            }
            assert_eq!(
                final_tasks.len(),
                SEEDED + ADDERS,
                "iter {iter}: final task count must equal seeded + added, no task dropped"
            );

            // (b) No two next_claim winners ever won the SAME task id.
            let winners = winners.lock().unwrap();
            let mut sorted = winners.clone();
            sorted.sort();
            let mut deduped = sorted.clone();
            deduped.dedup();
            assert_eq!(
                sorted.len(),
                deduped.len(),
                "iter {iter}: two next_claim callers won the same task id (lost-update race); \
                 winners = {sorted:?}"
            );

            // Every winner must be persisted as `claimed` (not clobbered by a
            // racing add's load-modify-save).
            for winner_id in winners.iter() {
                let status = final_tasks
                    .iter()
                    .find(|t| &t.id == winner_id)
                    .map(|t| t.status.as_str());
                assert_eq!(
                    status,
                    Some(STATUS_CLAIMED),
                    "iter {iter}: claimed task {winner_id} must persist as claimed"
                );
            }
        }
    }

    #[test]
    fn next_claim_excludes_already_claimed_task() {
        let path = tmp_path();
        add(&path, "Task", "/repo", vec![], "", 100).unwrap();
        let first = next_claim(&path, None, None).unwrap();
        assert!(first.is_some());
        assert_eq!(first.unwrap().status, "claimed");

        // A second claim call must NOT re-offer the already-claimed task.
        let second = next_claim(&path, None, None).unwrap();
        assert!(
            second.is_none(),
            "a claimed task must not be handed out again"
        );
    }

    #[test]
    fn next_excludes_claimed_task_too() {
        // Plain `next` also must not resurface a claimed task (claimed is not
        // `is_pending()`), so `next` and `next_claim` agree on what's eligible.
        let path = tmp_path();
        add(&path, "Task", "/repo", vec![], "", 100).unwrap();
        next_claim(&path, None, None).unwrap();
        assert!(next(&path, None, None).unwrap().is_none());
    }

    #[test]
    fn next_claim_reclaims_stale_claim() {
        let path = tmp_path();
        let id = add(&path, "Task", "/repo", vec![], "", 100).unwrap();
        next_claim(&path, None, None).unwrap();

        // Force the claim to look old by rewriting updated_at directly.
        let mut tasks = load(&path).unwrap();
        tasks[0].updated_at = 100; // far in the past relative to `now_unix()`
        save(&path, &tasks).unwrap();

        // A stale claim (older than CLAIM_STALE_SECS) must be reclaimable.
        let reclaimed = next_claim(&path, None, None).unwrap();
        assert!(
            reclaimed.is_some(),
            "a stale claim must be reclaimable, not stuck forever"
        );
        assert_eq!(reclaimed.unwrap().id, id);
    }

    /// FAIL-CLOSED claim gate (d6bf269f): when the exclusive tasks-lock is NOT
    /// held, the claim read-modify-write must DECLINE (return `Ok(None)`) rather
    /// than run unprotected and double-claim the same task to concurrent
    /// callers. Driven deterministically through the `locked` seam — a real
    /// lock-contention test would burn the ~8s acquire budget. RED against the
    /// old `with_tasks_lock` path, which ran the claim body unconditionally and
    /// would mark the task `claimed` even with no lock held.
    #[test]
    fn next_claim_declines_when_lock_not_held_instead_of_racing_unprotected() {
        let path = tmp_path();
        add(&path, "Task", "/repo", vec![], "", 100).unwrap();

        // Lock not held → must decline, and must NOT mutate the task on disk.
        let declined = claim_next_locked(&path, None, None, false).unwrap();
        assert!(
            declined.is_none(),
            "an unlocked claim must fail closed (decline), never race an unprotected RMW"
        );
        assert!(
            load(&path).unwrap()[0].is_pending(),
            "the task must stay pending when the claim was declined for lack of the lock"
        );

        // Sanity: with the lock held the same call claims normally.
        let claimed = claim_next_locked(&path, None, None, true).unwrap();
        assert!(
            claimed.is_some(),
            "with the lock held the claim proceeds as before"
        );
        assert_eq!(load(&path).unwrap()[0].status, "claimed");
    }

    #[test]
    fn next_claim_respects_filters_and_ordering() {
        let path = tmp_path();
        add(&path, "Low", "/repo", vec!["p2".into()], "", 100).unwrap();
        add(&path, "High", "/repo", vec!["p0".into()], "", 200).unwrap();
        let t = next_claim(&path, None, None).unwrap().unwrap();
        assert_eq!(t.title, "High");
    }

    #[test]
    fn next_claim_done_releases_and_completes() {
        // A normal claim → done lifecycle still works: mark_done acts on the
        // claimed task by id regardless of its current (claimed) status.
        let path = tmp_path();
        let id = add(&path, "Task", "/repo", vec![], "", 100).unwrap();
        let claimed = next_claim(&path, None, None).unwrap().unwrap();
        assert_eq!(claimed.id, id);
        mark_done(&path, &id).unwrap();
        let tasks = load(&path).unwrap();
        assert_eq!(tasks[0].status, "done");
    }

    // --- CA-backlog-002: is_claimed_elsewhere must be bounded ---------------

    /// F→P regression oracle for CA-backlog-002. Simulates a hung/slow
    /// `condukt state is-claimed` subprocess (here, literally shelling out to a
    /// process that sleeps far longer than IS_CLAIMED_TIMEOUT) via
    /// `run_with_bounded_wait` directly, and asserts the bounded wait gives up
    /// (returns `None`) well under `TASKS_LOCK_STALE_SECS` (10s) — proving the
    /// call cannot itself hold the tasks-file lock's critical section past a
    /// bound shorter than the stale-reap window.
    #[test]
    fn bounded_wait_gives_up_before_stale_reap_window() {
        // A subprocess that sleeps far longer than both IS_CLAIMED_TIMEOUT and
        // TASKS_LOCK_STALE_SECS, to prove the bound is enforced rather than
        // incidentally fast.
        let mut child = std::process::Command::new("sh")
            .arg("-c")
            .arg("sleep 30")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn sleep 30");

        let start = std::time::Instant::now();
        let result = run_with_bounded_wait(
            &mut child,
            Duration::from_millis(300),
            Duration::from_millis(5),
        );
        let elapsed = start.elapsed();

        assert!(
            result.is_none(),
            "a hung subprocess must be treated as timed-out (None), not waited on forever"
        );
        assert!(
            elapsed < Duration::from_secs(1),
            "CA-backlog-002: bounded wait took {elapsed:?}, must give up well under \
             TASKS_LOCK_STALE_SECS ({TASKS_LOCK_STALE_SECS}s)"
        );

        // Best-effort reap: the child must have been killed, not left hanging.
        let _ = child.kill();
        let _ = child.wait();
    }

    /// End-to-end version of the same oracle through the real fail-soft
    /// surface: `is_claimed_elsewhere` shells out to a `condukt` binary that,
    /// in this test, we make resolve (via PATH override) to a slow/hanging
    /// script instead of the real `condukt`. The call must return `false`
    /// ("not claimed", fail-open per spec) within well under
    /// `TASKS_LOCK_STALE_SECS`, never blocking on the hang.
    #[test]
    fn is_claimed_elsewhere_does_not_block_past_lock_stale_window_on_hang() {
        let _g = PROBE_PATH_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().expect("tmp dir");
        let fake_condukt = dir.path().join("condukt");
        std::fs::write(
            &fake_condukt,
            "#!/bin/sh\nsleep 30\nexit 0\n", // would report "claimed" if ever awaited
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&fake_condukt).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&fake_condukt, perms).unwrap();
        }

        let old_path = std::env::var("PATH").unwrap_or_default();
        // Prepend the fake `condukt` dir so it's found first.
        let new_path = format!("{}:{}", dir.path().display(), old_path);
        std::env::set_var("PATH", &new_path);

        let start = std::time::Instant::now();
        let claimed = is_claimed_elsewhere("deadbeefcafef00d");
        let elapsed = start.elapsed();

        std::env::set_var("PATH", old_path);

        assert!(
            !claimed,
            "a hung condukt subprocess must fail-open to 'not claimed'"
        );
        assert!(
            elapsed < Duration::from_secs(1),
            "CA-backlog-002: is_claimed_elsewhere took {elapsed:?} against a hung subprocess, \
             must give up well under TASKS_LOCK_STALE_SECS ({TASKS_LOCK_STALE_SECS}s) so it \
             cannot hold with_tasks_lock's critical section past the stale-reap window"
        );
    }

    // --- CA-backlog-001 / 003: durable, collision-free atomic save ----------

    /// F→P regression oracle for CA-backlog-003. Many threads concurrently call
    /// the RAW `save()` (bypassing `with_tasks_lock`, i.e. the fail-soft
    /// "degraded writer" path) on the SAME file. With the old FIXED temp
    /// filename (`.tasks.toml.tmp`) two writers shared one temp: one `rename`
    /// moved the temp out from under another, whose own `rename` then failed
    /// with ENOENT — so at least one `save` returned `Err` under contention.
    /// With a UNIQUE per-writer temp name, every writer owns its temp and no
    /// save ever errors. Reliably RED before the fix, GREEN after.
    #[test]
    fn save_unique_temp_no_collision_under_concurrency() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::{Arc, Barrier};

        for iter in 0..20 {
            let path = Arc::new(tmp_path());
            // Seed so the file exists (each writer overwrites with its own set).
            save(path.as_path(), &[]).unwrap();

            const N: usize = 16;
            let barrier = Arc::new(Barrier::new(N));
            let errors = Arc::new(AtomicUsize::new(0));

            let mut handles = Vec::with_capacity(N);
            for w in 0..N {
                let path = Arc::clone(&path);
                let barrier = Arc::clone(&barrier);
                let errors = Arc::clone(&errors);
                handles.push(std::thread::spawn(move || {
                    // Each writer persists a distinct, differently-sized set to
                    // widen the interleave window.
                    let mut set = Vec::new();
                    for i in 0..(w + 1) {
                        set.push(Task {
                            id: format!("w{w}-{i}"),
                            title: format!("writer-{w}-task-{i}"),
                            project: "/repo".to_string(),
                            project_unresolved: false,
                            tags: vec![],
                            status: STATUS_PENDING.to_string(),
                            notes: String::new(),
                            created_at: 100,
                            updated_at: 100,
                            defer_until: None,
                            weight: 0.0,
                            issue_number: None,
                            issue_url: None,
                        });
                    }
                    barrier.wait();
                    if save(path.as_path(), &set).is_err() {
                        errors.fetch_add(1, Ordering::SeqCst);
                    }
                }));
            }
            for h in handles {
                h.join().expect("thread join");
            }

            assert_eq!(
                errors.load(Ordering::SeqCst),
                0,
                "iter {iter}: a concurrent raw save errored (fixed-temp collision → rename ENOENT)"
            );
        }
    }

    /// F→P regression oracle for CA-backlog-001 (durable atomic save). Drives
    /// the save through the [`DurabilitySyncer`] seam with a recording observer
    /// and asserts the EXACT durability ORDERING: the temp file is fsync'd
    /// BEFORE the rename and the parent directory is fsync'd AFTER the rename
    /// (`WriteTmp → SyncTmp → Rename → SyncDir`). This discriminates on the
    /// fsync steps THEMSELVES, not on the unique temp-naming (CA-003): if the
    /// two production fsync calls (`sync_file` before rename, `sync_dir` after)
    /// are removed — the pre-fix write+rename state — the recorded sequence
    /// loses `SyncTmp`/`SyncDir` and this assertion FAILS. Verified RED with
    /// both fsync calls removed, GREEN with them present.
    #[test]
    fn save_fsyncs_temp_before_rename_and_dir_after() {
        use std::cell::RefCell;

        struct RecordingSyncer {
            steps: RefCell<Vec<SaveStep>>,
        }
        impl DurabilitySyncer for RecordingSyncer {
            fn on_step(&self, step: SaveStep) {
                self.steps.borrow_mut().push(step);
            }
            fn sync_file(&self, _f: &std::fs::File) -> std::io::Result<()> {
                // Recording the step here (not via a separate hook) is what
                // couples the record to the CALL: if production stops invoking
                // `sync_file`, `SyncTmp` is never recorded.
                self.steps.borrow_mut().push(SaveStep::SyncTmp);
                Ok(())
            }
            fn sync_dir(&self, _dir: &Path) {
                self.steps.borrow_mut().push(SaveStep::SyncDir);
            }
        }

        let path = tmp_path();
        let rec = RecordingSyncer {
            steps: RefCell::new(Vec::new()),
        };
        save_with_syncer(
            &path,
            &[Task {
                id: "aaaa1111".to_string(),
                title: "A".to_string(),
                project: "/repo".to_string(),
                project_unresolved: false,
                tags: vec![],
                status: STATUS_PENDING.to_string(),
                notes: String::new(),
                created_at: 100,
                updated_at: 100,
                defer_until: None,
                weight: 0.0,
                issue_number: None,
                issue_url: None,
            }],
            &rec,
        )
        .unwrap();

        assert_eq!(
            *rec.steps.borrow(),
            vec![
                SaveStep::WriteTmp,
                SaveStep::SyncTmp,
                SaveStep::Rename,
                SaveStep::SyncDir,
            ],
            "CA-backlog-001: durable save must fsync the temp BEFORE the rename \
             and fsync the parent dir AFTER the rename"
        );

        // The file must also actually round-trip (the seam did not skip the
        // real write/rename).
        let loaded = load(&path).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, "aaaa1111");
    }

    // --- CA-backlog-002: a claimed task blocks a duplicate re-add -----------

    /// F→P regression oracle for CA-backlog-002. Once a task is `claimed`
    /// (reserved, in-progress work), re-adding the same content must be
    /// rejected as a duplicate. Before the fix `check_duplicate` blocked only
    /// `pending`/`failed`, so the `claimed` task did NOT block the re-add and a
    /// duplicate of in-flight work slipped in. RED before, GREEN after.
    #[test]
    fn add_rejects_duplicate_of_claimed_task() {
        let path = tmp_path();
        add(&path, "Fix login", "/repo", vec![], "", 100).unwrap();
        // Reserve it via next_claim → status becomes `claimed`.
        let claimed = next_claim(&path, None, None).unwrap().unwrap();
        assert_eq!(claimed.status, "claimed");

        let err = add(&path, "Fix login", "/repo", vec![], "", 200)
            .expect_err("a claimed (in-progress) duplicate must be rejected");
        assert!(err.to_string().contains("duplicate"), "got: {err}");
        // Only the original task exists — no duplicate was persisted.
        assert_eq!(load(&path).unwrap().len(), 1);
    }

    // --- CA-backlog-004: edit validates --status like list ------------------

    /// F→P regression oracle for CA-backlog-004. `edit(status = "open")` — a
    /// typo (`open` is hypothesis's vocabulary, never backlog's) — must be
    /// REJECTED, leaving the task in its prior valid state so `next` can still
    /// surface it. Before the fix the raw `open` was written through, stranding
    /// the task out of `next`/`next_claim` with `requeue_expired` unable to
    /// rescue it. RED before, GREEN after.
    #[test]
    fn edit_rejects_unknown_status() {
        let path = tmp_path();
        let id = add(&path, "Task", "/repo", vec![], "", 100).unwrap();

        let err = edit(&path, &id, None, None, None, Some("open"))
            .expect_err("an unknown status must be rejected, not written through");
        assert!(
            err.to_string().contains("unknown status 'open'"),
            "got: {err}"
        );

        // The task keeps its prior valid status and is still pickable.
        let tasks = load(&path).unwrap();
        assert_eq!(tasks[0].status, "pending", "status must be left unchanged");
        assert!(
            next(&path, None, None).unwrap().is_some(),
            "the task must not be stranded out of the queue by a rejected edit"
        );

        // A VALID status still edits fine (fix must not over-reject).
        edit(&path, &id, None, None, None, Some("done")).expect("a valid status must be accepted");
        assert_eq!(load(&path).unwrap()[0].status, "done");
    }

    // --- CA-backlog-005: requeue_expired rescues a stale claimed task -------

    /// F→P regression oracle for CA-backlog-005. A `claimed` task whose
    /// claimant crashed (claim older than CLAIM_STALE_SECS) must be rescued to
    /// `pending` by `requeue_expired` — the unattended SessionStart path — not
    /// only by `next_claim`. Before the fix `requeue_expired` ignored `claimed`
    /// tasks entirely, so a crashed claimant stranded its task in `claimed`
    /// forever (plain `next` never resurfaced it). RED before, GREEN after.
    #[test]
    fn requeue_expired_rescues_stale_claimed_task() {
        let path = tmp_path();

        // Seed a claimed task whose claim is far older than CLAIM_STALE_SECS
        // relative to `now`, plus a FRESH claimed task that must NOT be rescued.
        let now = 10_000_000i64;
        let seed = vec![
            Task {
                id: "stale".to_string(),
                title: "stale-claim".to_string(),
                project: "/repo".to_string(),
                project_unresolved: false,
                tags: vec![],
                status: STATUS_CLAIMED.to_string(),
                notes: String::new(),
                created_at: 100,
                updated_at: now - CLAIM_STALE_SECS - 1, // past TTL
                defer_until: None,
                weight: 0.0,
                issue_number: None,
                issue_url: None,
            },
            Task {
                id: "fresh".to_string(),
                title: "fresh-claim".to_string(),
                project: "/repo".to_string(),
                project_unresolved: false,
                tags: vec![],
                status: STATUS_CLAIMED.to_string(),
                notes: String::new(),
                created_at: 100,
                updated_at: now - 1, // well within TTL
                defer_until: None,
                weight: 0.0,
                issue_number: None,
                issue_url: None,
            },
        ];
        save(&path, &seed).unwrap();

        let count = requeue_expired(&path, now).unwrap();
        assert_eq!(count, 1, "exactly the stale claimed task must be rescued");

        let tasks = load(&path).unwrap();
        let stale = tasks.iter().find(|t| t.id == "stale").unwrap();
        let fresh = tasks.iter().find(|t| t.id == "fresh").unwrap();
        assert_eq!(
            stale.status, "pending",
            "a stale claimed task must be rescued to pending by requeue_expired"
        );
        assert_eq!(
            fresh.status, "claimed",
            "a fresh (within-TTL) claimed task must NOT be rescued (no over-rescue)"
        );

        // And plain `next` (not just next_claim) can now resurface it.
        let picked = next(&path, None, None).unwrap().unwrap();
        assert_eq!(picked.id, "stale");
    }

    // ---- add_with_weight_and_github_push (backlog-add-integration) ---------

    /// Case 1: a GitHub-managed project + a successful `gh issue create` (via
    /// the injected runner) must record issue_number/issue_url on the saved
    /// Task, persisted to tasks.toml.
    #[test]
    fn github_push_records_issue_ref_on_success() {
        let path = tmp_path();
        let id = add_with_weight_and_github_push(
            &path,
            "Add feature X",
            "/repo",
            vec![],
            "some body",
            0.0,
            false,
            1000,
            "https://github.com/owner/repo.git",
            |argv| {
                assert_eq!(
                    argv,
                    [
                        "issue",
                        "create",
                        "--title",
                        "Add feature X",
                        "--body",
                        "some body"
                    ]
                );
                Some((
                    true,
                    "https://github.com/owner/repo/issues/42\n".to_string(),
                ))
            },
        )
        .unwrap();

        let tasks = load(&path).unwrap();
        let task = tasks.iter().find(|t| t.id == id).unwrap();
        assert_eq!(task.issue_number, Some(42));
        assert_eq!(
            task.issue_url.as_deref(),
            Some("https://github.com/owner/repo/issues/42")
        );
    }

    /// Case 2: a non-GitHub-managed project must skip the issue-create call
    /// entirely (fail-soft `decide_issue_create` short-circuit) and the add
    /// must still succeed locally with issue fields left None.
    #[test]
    fn github_push_skipped_for_non_github_remote_add_still_succeeds() {
        let path = tmp_path();
        let id = add_with_weight_and_github_push(
            &path,
            "Task",
            "/repo",
            vec![],
            "",
            0.0,
            false,
            1000,
            "https://gitlab.com/owner/repo.git",
            |_argv| panic!("gh must not be invoked for a non-github remote"),
        )
        .unwrap();

        let tasks = load(&path).unwrap();
        let task = tasks.iter().find(|t| t.id == id).unwrap();
        assert!(task.issue_number.is_none());
        assert!(task.issue_url.is_none());
    }

    /// Case 3: `gh` absent (injected runner always returns None) must
    /// fail-soft — `backlog add` still succeeds, issue fields stay None.
    #[test]
    fn github_push_gh_absent_add_still_succeeds() {
        let path = tmp_path();
        let id = add_with_weight_and_github_push(
            &path,
            "Task",
            "/repo",
            vec![],
            "",
            0.0,
            false,
            1000,
            "https://github.com/owner/repo.git",
            |_argv| None,
        )
        .unwrap();

        let tasks = load(&path).unwrap();
        let task = tasks.iter().find(|t| t.id == id).unwrap();
        assert!(task.issue_number.is_none());
        assert!(task.issue_url.is_none());
    }

    /// Case 4: `gh issue create` runs but exits non-zero — must fail-soft the
    /// same way, add still succeeds locally, issue fields stay None.
    #[test]
    fn github_push_gh_failure_add_still_succeeds() {
        let path = tmp_path();
        let id = add_with_weight_and_github_push(
            &path,
            "Task",
            "/repo",
            vec![],
            "",
            0.0,
            false,
            1000,
            "https://github.com/owner/repo.git",
            |_argv| Some((false, "HTTP 422: validation failed".to_string())),
        )
        .unwrap();

        let tasks = load(&path).unwrap();
        let task = tasks.iter().find(|t| t.id == id).unwrap();
        assert!(task.issue_number.is_none());
        assert!(task.issue_url.is_none());
    }

    /// The existing guards (duplicate-content rejection, bare-label ambiguity)
    /// must still apply on this new entry point, not just on `add_with_weight`.
    #[test]
    fn github_push_still_rejects_duplicate_content() {
        let path = tmp_path();
        add_with_weight_and_github_push(
            &path,
            "Same title",
            "/repo",
            vec![],
            "",
            0.0,
            false,
            1000,
            "https://gitlab.com/owner/repo.git",
            |_argv| panic!("must not be invoked for a non-github remote"),
        )
        .unwrap();

        let err = add_with_weight_and_github_push(
            &path,
            "Same title",
            "/repo",
            vec![],
            "",
            0.0,
            false,
            2000,
            "https://gitlab.com/owner/repo.git",
            |_argv| panic!("must not be invoked for a non-github remote"),
        )
        .unwrap_err();
        assert!(err.to_string().contains("duplicate"));
    }
}
