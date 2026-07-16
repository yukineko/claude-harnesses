use std::path::{Path, PathBuf};

use harness_core::config::home;
// Shared with condukt (the single source of truth) so autoflow reads the exact
// run-state directory condukt writes — see harness_core::projkey.
use harness_core::projkey::{project_key, repo_root};
use serde::{Deserialize, Serialize};

/// 2 hours in seconds. Running tasks older than this are considered interrupted.
const STUCK_SECS: i64 = 7200;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskState {
    pub id: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<i64>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct RunState {
    #[serde(default)]
    pub run_id: String,
    #[serde(default)]
    pub goal: String,
    #[serde(default)]
    pub tasks: Vec<TaskState>,
}

/// Find pending/failed tasks for the repo containing `cwd`.
///
/// Before returning, reverts any `running` task whose `updated_at` is older
/// than 2 hours — those were almost certainly interrupted mid-session.
pub fn find_pending(cwd: &Path) -> Vec<TaskState> {
    let (path, mut run) = match load_latest(cwd) {
        Some(x) => x,
        None => return vec![],
    };

    let now = now_secs();
    let mut modified = false;
    for task in &mut run.tasks {
        if task.status == "running" {
            let age = task.updated_at.map(|t| now - t).unwrap_or(i64::MAX);
            if age > STUCK_SECS {
                task.status = "pending".to_string();
                task.updated_at = None;
                modified = true;
            }
        }
    }
    if modified {
        if let Err(e) = save_run(&path, &run) {
            eprintln!(
                "autoflow: failed to persist condukt run state to {}: {e}",
                path.display()
            );
        }
    }

    run.tasks
        .into_iter()
        .filter(|t| matches!(t.status.as_str(), "pending" | "failed"))
        .collect()
}

/// Did the most recent condukt run for the repo containing `cwd` reach a
/// terminal state (`verified` or `failed`) on at least one task? Used by the
/// Tier 2 delegation-record advisory to tell "a condukt run completed this
/// session" apart from "nothing has finished yet". Fail-soft: no run state on
/// disk (or an unreadable/unparseable one) returns `false`.
pub fn has_completed_tasks(cwd: &Path) -> bool {
    match load_latest(cwd) {
        Some((_, run)) => run
            .tasks
            .iter()
            .any(|t| matches!(t.status.as_str(), "verified" | "failed")),
        None => false,
    }
}

/// Mark the given task IDs as `running` (with current timestamp) in the most
/// recent condukt run for the repo containing `cwd`.
pub fn mark_running(cwd: &Path, task_ids: &[&str]) {
    let (path, mut run) = match load_latest(cwd) {
        Some(x) => x,
        None => return,
    };
    let now = now_secs();
    let mut modified = false;
    for task in &mut run.tasks {
        if task_ids.contains(&task.id.as_str())
            && matches!(task.status.as_str(), "pending" | "failed")
        {
            task.status = "running".to_string();
            task.updated_at = Some(now);
            modified = true;
        }
    }
    if modified {
        if let Err(e) = save_run(&path, &run) {
            eprintln!(
                "autoflow: failed to persist condukt run state to {}: {e}",
                path.display()
            );
        }
    }
}

fn load_latest(cwd: &Path) -> Option<(PathBuf, RunState)> {
    let root = repo_root(cwd);
    let key = project_key(&root);
    let project_dir = home().join(".condukt").join("state").join(&key);
    let path = latest_run_file(&project_dir)?;
    let run = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str::<RunState>(&t).ok())?;
    Some((path, run))
}

/// Persist run-state. Returns the IO/serialize error instead of swallowing it,
/// so a failed save can no longer leave callers acting on a stale on-disk state
/// (which would re-mark or lose tasks). Writes atomically (tmp→rename) to match
/// condukt's own `RunState::save` and avoid a torn file under a concurrent read.
fn save_run(path: &Path, run: &RunState) -> std::io::Result<()> {
    let text = serde_json::to_string_pretty(run)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn latest_run_file(project_dir: &Path) -> Option<PathBuf> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(project_dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with("run-") && n.ends_with(".json"))
                .unwrap_or(false)
        })
        .collect();
    entries.sort();
    entries.pop()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression guard against re-duplication: autoflow must derive the SAME
    /// project key as the shared source of truth (which condukt also uses). If a
    /// future change reintroduces a private copy here, this breaks.
    #[test]
    fn project_key_matches_shared_source() {
        let p = Path::new("/tmp/some-repo");
        assert_eq!(project_key(p), harness_core::projkey::project_key(p));
    }

    // `has_completed_tasks` reads `$HOME/.condukt/state/...`, so these tests
    // mutate the process-global HOME and serialize behind the crate-wide
    // `test_home_guard` mutex (shared with main.rs's/lock.rs's own tests) to
    // avoid a cross-test HOME race.

    /// A temp HOME with a fake repo (`.git/`) and its condukt run-state dir
    /// pre-resolved, so tests can write a `run-*.json` straight into it.
    struct TmpEnv {
        _dir: tempfile::TempDir,
        repo: PathBuf,
        run_dir: PathBuf,
        _guard: std::sync::MutexGuard<'static, ()>,
    }
    impl TmpEnv {
        fn new() -> Self {
            let guard = crate::test_home_guard();
            let dir = tempfile::tempdir().expect("tempdir");
            let home = dir.path().to_path_buf();
            std::env::set_var("HOME", &home);

            let repo = home.join("repo");
            std::fs::create_dir_all(repo.join(".git")).unwrap();

            let key = project_key(&repo_root(&repo));
            let run_dir = home.join(".condukt").join("state").join(&key);
            std::fs::create_dir_all(&run_dir).unwrap();

            TmpEnv {
                _dir: dir,
                repo,
                run_dir,
                _guard: guard,
            }
        }

        fn write_run(&self, tasks_json: &str) {
            let text = format!(r#"{{"run_id":"r1","goal":"g","tasks":{tasks_json}}}"#);
            std::fs::write(self.run_dir.join("run-0001.json"), text).unwrap();
        }
    }

    #[test]
    fn has_completed_tasks_true_when_a_task_is_verified() {
        let env = TmpEnv::new();
        env.write_run(r#"[{"id":"t1","status":"verified"}]"#);
        assert!(has_completed_tasks(&env.repo));
    }

    #[test]
    fn has_completed_tasks_true_when_a_task_is_failed() {
        let env = TmpEnv::new();
        env.write_run(r#"[{"id":"t1","status":"failed"}]"#);
        assert!(has_completed_tasks(&env.repo));
    }

    #[test]
    fn has_completed_tasks_false_when_only_pending() {
        let env = TmpEnv::new();
        env.write_run(r#"[{"id":"t1","status":"pending"},{"id":"t2","status":"running"}]"#);
        assert!(!has_completed_tasks(&env.repo));
    }

    #[test]
    fn has_completed_tasks_false_when_no_run_state() {
        let env = TmpEnv::new();
        // No run-*.json written at all.
        assert!(!has_completed_tasks(&env.repo));
    }
}
