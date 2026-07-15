use anyhow::{anyhow, Context, Result};
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

/// Vec<Task> を tasks.toml に書き戻す (アトミック書き込み: 一時ファイル→rename)。
pub fn save(path: &Path, tasks: &[Task]) -> Result<()> {
    let file = TasksFile {
        task: tasks.to_vec(),
    };
    let text = toml::to_string_pretty(&file).context("failed to serialize tasks to TOML")?;

    // 一時ファイルは同ディレクトリに置いて rename でアトミック差し替え
    let tmp_path = path.with_file_name(".tasks.toml.tmp");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create directory {}", parent.display()))?;
    }
    std::fs::write(&tmp_path, &text)
        .with_context(|| format!("failed to write tmp file {}", tmp_path.display()))?;
    std::fs::rename(&tmp_path, path).with_context(|| {
        format!(
            "failed to rename {} -> {}",
            tmp_path.display(),
            path.display()
        )
    })?;
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
/// Bounded blocking-acquire budget: attempts × sleep ≈ 2s worst case.
///
/// CA-backlog-003: must comfortably exceed [`IS_CLAIMED_TIMEOUT`] (300ms) —
/// the longest a live holder can legitimately keep the lock — so a waiting
/// racer never gives up on ACQUIRING while the current holder is still
/// legitimately mid-critical-section, and never itself needs to treat that
/// live holder's lockfile as stale to make progress.
const TASKS_LOCK_MAX_ATTEMPTS: u32 = 400;
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
/// within the budget — the caller must then degrade to a best-effort unprotected
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

/// タスクを追加して保存。生成した id を返す。weight は 0.0 (= 既定の優先順位)。
/// weight を明示したい呼び出し元は [`add_with_weight`] を使う。
///
/// バイナリ側は `--weight` を取れる [`add_with_weight`] を直接呼ぶため、この
/// 0.0 既定ラッパーはテスト専用 (`#[cfg(test)]`)。
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
    with_tasks_lock(path, || {
        let mut tasks = load(path)?;
        if !force {
            check_duplicate(&tasks, title, project)?;
        }
        let id = new_id(title, now);
        let task = Task {
            id: id.clone(),
            title: title.to_string(),
            project: project.to_string(),
            tags,
            status: STATUS_PENDING.to_string(),
            notes: notes.to_string(),
            created_at: now,
            updated_at: now,
            defer_until: None,
            weight,
        };
        tasks.push(task);
        save(path, &tasks)?;
        Ok(id)
    })
}

/// Reject an add whose content [`crate::task::hashkey`] is already held by
/// EITHER (a) an existing task in `tasks` with status `pending` or `failed`
/// (a `done` task with the same title does NOT block a re-add), OR (b) a live
/// cross-session claim reported by `condukt state is-claimed --hashkey <h>`.
///
/// Fail-soft on (b): if the `condukt` binary is absent from PATH, or the
/// command errors or exits with anything other than 0 (claimed) / 1 (not
/// claimed), this is treated as "not claimed" — a missing or misbehaving
/// `condukt` must never block `backlog add`.
fn check_duplicate(tasks: &[Task], title: &str, project: &str) -> Result<()> {
    let hk = crate::task::hashkey(title, project);

    if tasks.iter().any(|t| {
        crate::task::hashkey(&t.title, &t.project) == hk
            && matches!(t.status.as_str(), STATUS_PENDING | STATUS_FAILED)
    }) {
        return Err(anyhow!(
            "duplicate task rejected: an existing pending/failed task already has this content (hashkey {hk}); use --force to add anyway"
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
    Ok(pick_next(&tasks, now, tag_filter, project_filter).map(|t| (*t).clone()))
}

/// Selects the single highest-priority eligible task from `tasks` (shared by
/// [`next`] and [`next_claim`] so both use IDENTICAL candidate-selection and
/// ordering logic).
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
    with_tasks_lock(path, || {
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
    })
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
            if let Some(defer_until) = task.defer_until {
                if defer_until <= now {
                    task.defer_until = None;
                    task.status = STATUS_PENDING.to_string();
                    task.updated_at = now;
                    count += 1;
                }
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
pub fn edit(
    path: &Path,
    id: &str,
    title: Option<&str>,
    tags: Option<Vec<String>>,
    notes: Option<&str>,
    status: Option<&str>,
) -> Result<()> {
    with_tasks_lock(path, || {
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

/// タスク一覧を返す。フィルタは all None で全件。
pub fn list(
    path: &Path,
    tag_filter: Option<&str>,
    project_filter: Option<&str>,
    status_filter: Option<&str>,
) -> Result<Vec<Task>> {
    let tasks = load(path)?;
    let mut result: Vec<Task> = tasks
        .into_iter()
        .filter(|t| match tag_filter {
            Some(tag) => t.tags.iter().any(|tg| tg == tag),
            None => true,
        })
        .filter(|t| match project_filter {
            Some(proj) => project_matches(&t.project, proj),
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

/// project_filter のマッチング:
/// Task.project が filter と完全一致、または filter で始まる場合にマッチ。
fn project_matches(task_project: &str, filter: &str) -> bool {
    if task_project == filter {
        return true;
    }
    // filter が末尾スラッシュなしの repo_root の場合を考慮
    // task_project が filter + "/" で始まればマッチ
    if let Some(rest) = task_project.strip_prefix(filter) {
        return rest.starts_with('/');
    }
    false
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
                    tags: vec![],
                    status: STATUS_FAILED.to_string(),
                    notes: String::new(),
                    created_at: 100,
                    updated_at: 100,
                    defer_until: Some(500),
                    weight: 0.0,
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
                    tags: vec![],
                    status: STATUS_PENDING.to_string(),
                    notes: String::new(),
                    created_at: 100,
                    updated_at: 100,
                    defer_until: None,
                    weight: 0.0,
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
}
