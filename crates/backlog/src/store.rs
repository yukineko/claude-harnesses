use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::task::{new_id, Task, STATUS_DONE, STATUS_FAILED, STATUS_PENDING};

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
/// The critical section is a single load-modify-save (sub-millisecond), so a
/// live holder is never falsely reaped.
const TASKS_LOCK_STALE_SECS: u64 = 5;
/// Bounded blocking-acquire budget: attempts × sleep ≈ 150ms worst case.
const TASKS_LOCK_MAX_ATTEMPTS: u32 = 50;
const TASKS_LOCK_SLEEP: Duration = Duration::from_millis(3);

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

/// Fail-soft check: does a live cross-session claim (from any `condukt` run)
/// hold this hashkey? Shells out to `condukt state is-claimed --hashkey <h>`
/// (exit 0 = claimed, exit 1 = not claimed). Any other outcome — `condukt`
/// missing from PATH, spawn failure, unexpected exit code — is treated as
/// "not claimed" so a stale or absent `condukt` never blocks `backlog add`.
fn is_claimed_elsewhere(hashkey: &str) -> bool {
    std::process::Command::new("condukt")
        .arg("state")
        .arg("is-claimed")
        .arg("--hashkey")
        .arg(hashkey)
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

/// pending/failed タスクを優先度順 (priority() 昇順、同優先度は created_at 昇順) で返す。
/// tag_filter: Some(tag) なら tags にそのタグを含むものだけ。
/// project_filter: Some(project) ならプロジェクトが一致するものだけ (repo_root との比較)。
/// defer_until が未来のタスク (is_deferred) はスキップする。
pub fn next(
    path: &Path,
    tag_filter: Option<&str>,
    project_filter: Option<&str>,
) -> Result<Option<Task>> {
    let now = now_unix();
    let tasks = load(path)?;
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
    Ok(candidates.first().map(|t| (*t).clone()))
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
pub fn mark_done(path: &Path, id: &str) -> Result<()> {
    with_tasks_lock(path, || {
        let mut tasks = load(path)?;
        let task = tasks
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or_else(|| anyhow!("task not found: {}", id))?;
        task.status = STATUS_DONE.to_string();
        // updated_at はシステム時刻で更新（呼び出し元が now を持たないため現在時刻を使う）
        task.updated_at = now_unix();
        save(path, &tasks)
    })
}

/// id で特定のタスクを failed に更新。reason を notes に追記。
/// defer_until を now + 172800 (2日) に設定してタスクを一時保留にする。
pub fn mark_failed(path: &Path, id: &str, reason: Option<&str>) -> Result<()> {
    with_tasks_lock(path, || {
        let mut tasks = load(path)?;
        let task = tasks
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or_else(|| anyhow!("task not found: {}", id))?;
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
        // A done task with the same title/project must NOT block a re-add.
        let new_id = add(&path, "Fix login", "/repo", vec![], "", 200)
            .expect("a done duplicate must not block a new add");
        let tasks = load(&path).unwrap();
        assert_eq!(tasks.len(), 2);
        assert!(tasks.iter().any(|t| t.id == new_id));
    }
}
