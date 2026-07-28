use std::path::{Path, PathBuf};

use harness_core::config::home;
// Shared with condukt (the single source of truth) so autoflow reads the exact
// run-state directory condukt writes — see harness_core::projkey.
use harness_core::projkey::{project_key, repo_root};
use harness_core::verdict::Determination;
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
///
/// Three answers, not two. `Known(vec![])` means the run-state was read and
/// holds no pending work; `Undetermined` means it could not be read (see
/// [`load_latest`]). The distinction is load-bearing: the caller latches
/// `Phase::Done` on an empty set, so an unreadable run file collapsing into an
/// empty vec makes "we couldn't look" indistinguishable from "there is nothing
/// left" — and a single task missing its `status` field is enough to make a run
/// full of healthy tasks unreadable (audit §4.4).
pub fn find_pending(cwd: &Path) -> Determination<Vec<TaskState>> {
    let (path, mut run) = match load_latest(cwd) {
        Determination::Known(Some(x)) => x,
        // No run file for this project: condukt was never run here. Observed.
        Determination::Known(None) => return Determination::Known(vec![]),
        Determination::Undetermined(why) => return Determination::Undetermined(why),
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

    Determination::Known(
        run.tasks
            .into_iter()
            .filter(|t| matches!(t.status.as_str(), "pending" | "failed"))
            .collect(),
    )
}

/// Did the SPECIFIC condukt run named `run_id` (for the repo containing `cwd`)
/// reach a terminal state (`verified` or `failed`) on at least one task?
///
/// Unlike a project-wide "most recent run file" lookup (which would follow
/// whatever run file sorts last project-wide, regardless of who wrote it),
/// this loads the exact run file this session claims to have driven. That
/// distinction matters for the Tier 2 delegation-record advisory: two condukt
/// sessions can run concurrently against the same project, and "the newest
/// run file on disk" can belong to a completely unrelated, still-running
/// session's run rather than this session's own. Scoping to a caller-supplied
/// `run_id` (extracted from this session's own transcript — see
/// `delegation_audit::extract_flow_run_ids`) removes that cross-session
/// false-positive path entirely.
///
/// Uses the same `<project_dir>/<safe_session(run_id)>.json` layout condukt's
/// own `run_path` writes (see `crates/condukt/src/state.rs`), so this reads
/// the exact file a matching `condukt state ... --run <run_id>` invocation
/// would have written. Fail-soft: an unreadable/unparseable/missing run file
/// returns `false`.
pub fn has_completed_tasks_for_run(cwd: &Path, run_id: &str) -> bool {
    let root = repo_root(cwd);
    let key = project_key(&root);
    let project_dir = home().join(".condukt").join("state").join(&key);
    let path = project_dir.join(format!(
        "{}.json",
        harness_core::store::safe_session(run_id)
    ));
    let run = match std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str::<RunState>(&t).ok())
    {
        Some(r) => r,
        None => return false,
    };
    run.tasks
        .iter()
        .any(|t| matches!(t.status.as_str(), "verified" | "failed"))
}

/// Mark the given task IDs as `running` (with current timestamp) in the most
/// recent condukt run for the repo containing `cwd`.
pub fn mark_running(cwd: &Path, task_ids: &[&str]) {
    let (path, mut run) = match load_latest(cwd) {
        Determination::Known(Some(x)) => x,
        Determination::Known(None) => return,
        // Bookkeeping, not a verdict: failing to mark tasks running cannot make
        // autoflow conclude anything, it only means the next Stop re-observes
        // the same pending set (which `decide_progress` surfaces as stalled
        // progress). Say so on stderr rather than failing silently.
        Determination::Undetermined(why) => {
            eprintln!("autoflow: could not mark condukt tasks running: {why}");
            return;
        }
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

/// Load this project's most recent condukt run-state.
///
/// `Known(None)` = there is no run file (condukt never ran for this project) —
/// an observation. `Undetermined` = a run file exists but we could not turn it
/// into a `RunState` (unreadable, truncated mid-write, or valid JSON that does
/// not fit the schema — e.g. one task missing `status`, which fails the WHOLE
/// document and would otherwise hide every healthy task alongside it).
fn load_latest(cwd: &Path) -> Determination<Option<(PathBuf, RunState)>> {
    let root = repo_root(cwd);
    let key = project_key(&root);
    let project_dir = home().join(".condukt").join("state").join(&key);
    let path = match latest_run_file(&project_dir) {
        Determination::Known(Some(p)) => p,
        Determination::Known(None) => return Determination::Known(None),
        Determination::Undetermined(why) => return Determination::Undetermined(why),
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) => {
            return Determination::undetermined(format!(
                "could not read condukt run-state {}: {e}",
                path.display()
            ))
        }
    };
    match serde_json::from_str::<RunState>(&text) {
        Ok(run) => Determination::Known(Some((path, run))),
        Err(e) => Determination::undetermined(format!(
            "could not parse condukt run-state {}: {e}",
            path.display()
        )),
    }
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

/// The newest `run-*.json` in `project_dir`.
///
/// A missing directory is `Known(None)` (condukt never ran for this project);
/// any other enumeration failure is `Undetermined`, because a directory we
/// could not list is not a directory we observed to be empty (audit §4.6: a
/// mode-111 state dir made a live pending run invisible).
fn latest_run_file(project_dir: &Path) -> Determination<Option<PathBuf>> {
    let dir = match std::fs::read_dir(project_dir) {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Determination::Known(None),
        Err(e) => {
            return Determination::undetermined(format!(
                "could not enumerate {}: {e}",
                project_dir.display()
            ))
        }
    };
    let mut entries: Vec<PathBuf> = Vec::new();
    for entry in dir {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                return Determination::undetermined(format!(
                    "could not read an entry of {}: {e}",
                    project_dir.display()
                ))
            }
        };
        let path = entry.path();
        let is_run_file = path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.starts_with("run-") && n.ends_with(".json"))
            .unwrap_or(false);
        if is_run_file {
            entries.push(path);
        }
    }
    entries.sort();
    Determination::Known(entries.pop())
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

        /// Write a run file under its own `run_id`-named path (the layout
        /// `has_completed_tasks_for_run` reads).
        fn write_named_run(&self, run_id: &str, tasks_json: &str) {
            let text = format!(r#"{{"run_id":"{run_id}","goal":"g","tasks":{tasks_json}}}"#);
            std::fs::write(
                self.run_dir.join(format!(
                    "{}.json",
                    harness_core::store::safe_session(run_id)
                )),
                text,
            )
            .unwrap();
        }
    }

    #[test]
    fn has_completed_tasks_for_run_true_when_that_run_verified() {
        let env = TmpEnv::new();
        env.write_named_run(
            "run-20260101-000000-1",
            r#"[{"id":"t1","status":"verified"}]"#,
        );
        assert!(has_completed_tasks_for_run(
            &env.repo,
            "run-20260101-000000-1"
        ));
    }

    #[test]
    fn has_completed_tasks_for_run_true_when_that_run_failed() {
        let env = TmpEnv::new();
        env.write_named_run(
            "run-20260101-000000-1",
            r#"[{"id":"t1","status":"failed"}]"#,
        );
        assert!(has_completed_tasks_for_run(
            &env.repo,
            "run-20260101-000000-1"
        ));
    }

    #[test]
    fn has_completed_tasks_for_run_false_when_only_pending() {
        let env = TmpEnv::new();
        env.write_named_run(
            "run-20260101-000000-1",
            r#"[{"id":"t1","status":"pending"},{"id":"t2","status":"running"}]"#,
        );
        assert!(!has_completed_tasks_for_run(
            &env.repo,
            "run-20260101-000000-1"
        ));
    }

    #[test]
    fn has_completed_tasks_for_run_false_for_unknown_run_id() {
        let env = TmpEnv::new();
        env.write_named_run(
            "run-20260101-000000-1",
            r#"[{"id":"t1","status":"verified"}]"#,
        );
        assert!(!has_completed_tasks_for_run(
            &env.repo,
            "run-20260101-000000-999-does-not-exist"
        ));
    }

    /// Regression for the cross-session false-positive (backlog d8051ed4):
    /// two condukt runs exist for the same project — one belonging to THIS
    /// session (still pending, not completed) and a concurrent OTHER
    /// session's run that happens to sort later and HAS completed. A
    /// project-wide "most recent run file" lookup (`load_latest`, which
    /// `find_pending`/`mark_running` use for their own purposes) would follow
    /// whatever sorts last regardless of who wrote it and wrongly attribute
    /// the other session's completion to this one. `has_completed_tasks_for_run`
    /// scoped to this session's own run id must not.
    #[test]
    fn has_completed_tasks_for_run_does_not_leak_across_sessions() {
        let env = TmpEnv::new();
        // This session's own run: still pending, nothing completed.
        env.write_named_run(
            "run-20260101-000000-1000",
            r#"[{"id":"t1","status":"pending"}]"#,
        );
        // A different, concurrent session's run for the SAME project: sorts
        // after the one above and has a verified task.
        env.write_named_run(
            "run-20260101-999999-2000",
            r#"[{"id":"t1","status":"verified"}]"#,
        );

        // The project-wide, cross-session-hazardous lookup (`load_latest`)
        // follows whatever sorts last — the other session's completed run —
        // so naively trusting it would wrongly read as "this session
        // completed something."
        let (_, latest) = match load_latest(&env.repo) {
            Determination::Known(Some(x)) => x,
            other => panic!("a run file exists and must be readable, got {other:?}"),
        };
        assert!(
            latest
                .tasks
                .iter()
                .any(|t| matches!(t.status.as_str(), "verified" | "failed")),
            "sanity check: the project-wide latest run is the OTHER session's completed one"
        );

        // But scoped to the run THIS session's own transcript actually
        // showed it driving, completion must correctly read as `false`.
        assert!(!has_completed_tasks_for_run(
            &env.repo,
            "run-20260101-000000-1000"
        ));
        // And scoped to the other session's run id, it correctly reads true
        // (proving the function isn't just always false — it's genuinely
        // scoped to the id given).
        assert!(has_completed_tasks_for_run(
            &env.repo,
            "run-20260101-999999-2000"
        ));
    }
}
