//! Liveness gate for the EXPLICIT `condukt state abandon --run R --task <id>`
//! reset (backlog `aaae849b`).
//!
//! The explicit arm resets a running/failed task to `pending`, after which the
//! skill re-dispatches a second worker onto the same branch. Without a liveness
//! check that second worker can land on top of a first worker that is still
//! writing (the stacked-commit incident). The `--all-stuck`, reaper and
//! reconcile paths have their own gates and are NOT routed through here.
//!
//! **Signal.** The worker's subagent transcript mtime. The task records the
//! worker's Task-tool `agentId` via `state set --agent-id`; Claude Code writes
//! that worker's transcript at
//! `$HOME/.claude/projects/<project-slug>/<session-id>/subagents/agent-<agentId>.jsonl`.
//! condukt knows neither the orchestrating session id nor (reliably) the
//! project path that session was started in, so EVERY `<project-slug>/<session-id>`
//! directory under `$HOME/.claude/projects` is probed. That covers the slug of
//! the run's cwd under both its literal and its canonicalized spelling (macOS
//! `/var` → `/private/var`), the main worktree root, and a session started in
//! any other directory. The walk is bounded by [`MAX_SESSION_DIRS`]; exceeding
//! it is undetermined, never "absent".
//!
//! **Window.** A transcript modified less than [`live_window_secs`] ago is
//! live: the run's `stuck_ttl_secs` (default 1800 s; config `stuck_ttl_secs`
//! or env `CONDUKT_STUCK_TTL_SECS`), floored at [`MIN_LIVE_WINDOW_SECS`] so a
//! zero/tiny TTL cannot switch the gate off. A future mtime counts as age 0.
//!
//! **Decision (fail-closed, CLAUDE.md §3):**
//! * no recorded agent id, no transcript anywhere, or the freshest transcript
//!   is at least the window old → proceed (the explicit arm stays the human
//!   override for a dead worker);
//! * a transcript younger than the window → refuse, naming its path and age;
//! * cannot determine (a transcript whose mtime cannot be read, a store/dir
//!   that cannot be listed or stat'ed for a reason other than ENOENT, `$HOME`
//!   unset, an agent id that is not a plain `[A-Za-z0-9_-]+` token, the walk
//!   bound exceeded) → refuse, like a live one.
//!
//! `--force <REASON>` (non-empty after trimming) overrides a refusal. The
//! reason is appended as one JSON line to `<run_dir>/<run>.abandon-force.jsonl`
//! BEFORE the reset proceeds and echoed on stderr; if that record cannot be
//! written the reset is refused.

use crate::config::Config;
use crate::state::{self, TaskState};
use anyhow::{bail, Context, Result};
use harness_core::verdict::Determination;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Floor for the liveness window, in seconds.
pub const MIN_LIVE_WINDOW_SECS: u64 = 300;

/// Upper bound on `<project>/<session>` directories the probe will visit.
/// More than this is reported as undetermined (refuse), never as "absent".
pub const MAX_SESSION_DIRS: usize = 100_000;

/// The liveness window for a run configured with `stuck_ttl_secs`.
pub fn live_window_secs(stuck_ttl_secs: u64) -> u64 {
    stuck_ttl_secs.max(MIN_LIVE_WINDOW_SECS)
}

/// What the transcript store says about one agent's transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Probe {
    /// The store was read cleanly and holds no `agent-<id>.jsonl` anywhere.
    Absent,
    /// The freshest matching transcript and its age in seconds.
    Found { path: PathBuf, age_secs: u64 },
}

/// Age of a transcript from its (possibly unreadable) mtime. An unreadable
/// mtime on a file that exists is undetermined — the caller treats it as live.
fn age_of(path: &Path, mtime: std::io::Result<SystemTime>, now: SystemTime) -> Determination<u64> {
    match mtime {
        Ok(m) => Determination::Known(now.duration_since(m).map(|d| d.as_secs()).unwrap_or(0)),
        Err(e) => Determination::undetermined(format!(
            "transcript {} exists but its mtime cannot be read: {e}",
            path.display()
        )),
    }
}

/// `read_dir` that maps ENOENT to an empty listing and any other error to
/// undetermined. Returns only sub-directories.
fn list_dirs(dir: &Path) -> Determination<Vec<PathBuf>> {
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Determination::Known(vec![]),
        Err(e) => {
            return Determination::undetermined(format!("cannot list {}: {e}", dir.display()));
        }
    };
    let mut out = Vec::new();
    for entry in rd {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                return Determination::undetermined(format!("cannot list {}: {e}", dir.display()));
            }
        };
        match entry.file_type() {
            Ok(ft) if ft.is_dir() => out.push(entry.path()),
            Ok(_) => {}
            Err(e) => {
                return Determination::undetermined(format!(
                    "cannot stat {}: {e}",
                    entry.path().display()
                ));
            }
        }
    }
    Determination::Known(out)
}

/// Probe every `<root>/<project>/<session>/subagents/agent-<agent_id>.jsonl`.
fn probe(root: &Path, agent_id: &str, now: SystemTime) -> Determination<Probe> {
    let valid = !agent_id.is_empty()
        && agent_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if !valid {
        return Determination::undetermined(format!(
            "recorded agent id {agent_id:?} is not a plain [A-Za-z0-9_-]+ token, so its transcript cannot be located"
        ));
    }
    let file_name = format!("agent-{agent_id}.jsonl");
    let projects = match list_dirs(root) {
        Determination::Known(p) => p,
        Determination::Undetermined(why) => return Determination::Undetermined(why),
    };
    let mut visited = 0usize;
    let mut freshest: Option<(PathBuf, u64)> = None;
    for project in projects {
        let sessions = match list_dirs(&project) {
            Determination::Known(s) => s,
            Determination::Undetermined(why) => return Determination::Undetermined(why),
        };
        for session in sessions {
            visited += 1;
            if visited > MAX_SESSION_DIRS {
                return Determination::undetermined(format!(
                    "more than {MAX_SESSION_DIRS} session dirs under {}; the transcript walk was not completed",
                    root.display()
                ));
            }
            let path = session.join("subagents").join(&file_name);
            let meta = match std::fs::metadata(&path) {
                Ok(m) => m,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => {
                    return Determination::undetermined(format!(
                        "cannot stat {}: {e}",
                        path.display()
                    ));
                }
            };
            let age = match age_of(&path, meta.modified(), now) {
                Determination::Known(a) => a,
                Determination::Undetermined(why) => return Determination::Undetermined(why),
            };
            if freshest.as_ref().is_none_or(|(_, best)| age < *best) {
                freshest = Some((path, age));
            }
        }
    }
    Determination::Known(match freshest {
        None => Probe::Absent,
        Some((path, age_secs)) => Probe::Found { path, age_secs },
    })
}

/// Why the explicit reset must not proceed without `--force`, or `None` to proceed.
fn refusal(root: Determination<PathBuf>, agent_id: &str, window: u64) -> Option<String> {
    let root = match root {
        Determination::Known(r) => r,
        Determination::Undetermined(why) => return Some(why.as_str().to_string()),
    };
    match probe(&root, agent_id, SystemTime::now()) {
        Determination::Known(Probe::Absent) => None,
        Determination::Known(Probe::Found { age_secs, .. }) if age_secs >= window => None,
        Determination::Known(Probe::Found { path, age_secs }) => Some(format!(
            "worker transcript {} was modified {age_secs}s ago (liveness window {window}s): the worker is live",
            path.display()
        )),
        Determination::Undetermined(why) => Some(format!(
            "worker liveness cannot be determined, treated as live: {}",
            why.as_str()
        )),
    }
}

/// Gate the explicit `state abandon --task` reset of `task`. `Ok(())` lets the
/// reset proceed; `Err` refuses it (non-zero exit). See the module docs.
pub fn check_explicit(
    cfg: &Config,
    cwd: &Path,
    run_id: &str,
    task: &TaskState,
    force: Option<&str>,
) -> Result<()> {
    if force.is_some_and(|r| r.trim().is_empty()) {
        bail!("--force requires a non-empty reason");
    }
    let Some(agent_id) = task.agent_id.as_deref() else {
        return Ok(());
    };
    let window = live_window_secs(cfg.stuck_ttl_secs);
    let Some(why) = refusal(
        crate::wt_reconcile::claude_projects_root(),
        agent_id,
        window,
    ) else {
        return Ok(());
    };
    let Some(reason) = force else {
        bail!(
            "refusing to abandon task '{}' (agent {agent_id}): {why}. A second worker would be \
             dispatched onto a live worker's branch. Wait until the transcript is idle, or pass \
             --force <REASON> if the worker is known dead.",
            task.id
        );
    };
    let journal = state::run_dir(cfg, cwd, run_id)?.join(format!("{run_id}.abandon-force.jsonl"));
    let line = serde_json::json!({
        "ts": state::now_secs(), "task": task.id, "agent_id": agent_id,
        "reason": reason, "overrode": why,
    });
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&journal)
        .and_then(|mut f| writeln!(f, "{line}"))
        .with_context(|| {
            format!(
                "cannot record the --force reason in {}; refusing",
                journal.display()
            )
        })?;
    eprintln!(
        "--force: abandoning task '{}' despite: {why}; reason recorded in {}: {reason}",
        task.id,
        journal.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use std::time::Duration;

    fn transcript(root: &Path, session: &str, id: &str) -> PathBuf {
        let dir = root.join("-proj").join(session).join("subagents");
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join(format!("agent-{id}.jsonl"));
        std::fs::write(&f, "{}\n").unwrap();
        f
    }

    #[test]
    fn unreadable_mtime_is_undetermined() {
        let e = std::io::Error::other("no mtime");
        let d = age_of(Path::new("/x"), Err(e), SystemTime::now());
        assert!(matches!(d, Determination::Undetermined(_)));
    }

    #[test]
    fn future_mtime_is_age_zero() {
        let now = SystemTime::now();
        let d = age_of(Path::new("/x"), Ok(now + Duration::from_secs(60)), now);
        assert_eq!(d, Determination::Known(0));
    }

    #[test]
    fn absent_store_and_absent_transcript_proceed() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(
            refusal(Determination::Known(tmp.path().join("nope")), "abc", 1800),
            None
        );
        transcript(tmp.path(), "s1", "other");
        assert_eq!(
            refusal(Determination::Known(tmp.path().to_path_buf()), "abc", 1800),
            None
        );
    }

    #[test]
    fn fresh_transcript_in_any_session_refuses_and_names_path() {
        let tmp = tempfile::tempdir().unwrap();
        let f = transcript(tmp.path(), "s2", "abc");
        let why = refusal(Determination::Known(tmp.path().to_path_buf()), "abc", 1800).unwrap();
        assert!(why.contains(&f.display().to_string()), "{why}");
    }

    #[test]
    fn unlocatable_agent_id_and_unset_home_refuse() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Determination::Known(tmp.path().to_path_buf());
        assert!(refusal(root, "../etc", 1800).is_some());
        assert!(refusal(Determination::undetermined("no HOME"), "abc", 1800).is_some());
    }

    #[test]
    fn window_has_a_floor() {
        assert_eq!(live_window_secs(0), MIN_LIVE_WINDOW_SECS);
        assert_eq!(live_window_secs(1800), 1800);
    }
}
