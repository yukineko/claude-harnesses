//! Verified-before-teardown gate for `condukt worktree remove` (backlog
//! `6180333a`, audit B item 12).
//!
//! Removing a task worktree before the task is verified deletes the tdd
//! proofs the F→P oracle reads, and leaves the operator with ENOENT instead of
//! a verdict. [`refuse_unverified_teardown`] is run by `worktree remove`
//! BEFORE anything is deleted.
//!
//! Which task owns the worktree:
//!
//! * the identity `branch.<b>.conduktTask = <run>/<task>` recorded by
//!   `worktree create --run` for the worktree's checked-out branch; when it is
//!   present that run/task is judged — a run that cannot be loaded or a task
//!   that is not in it is a refusal (cannot determine);
//! * otherwise every run-state file of the project is scanned for tasks whose
//!   recorded `worktree` is this path. The scan is strict: a state dir or
//!   state file that cannot be read or parsed is a refusal, not "no owner".
//!   No owning task anywhere (a worktree no condukt task records) proceeds.
//!
//! An owning task permits the teardown only when it is `verified`, or
//! deliberately resolved without its proofs (`cancelled`, `discarded`), or
//! `pending` with no recorded worktree — the outcome of the explicit
//! `condukt state abandon --run R --task T` reset (itself liveness-gated),
//! which is the override this refusal names. There is no bypass flag.
//! Every other status (`running`, `done`, `failed`, `pending` that still
//! records a worktree) refuses, naming the task id and its status.

use crate::config::Config;
use crate::state::{self, RunState, Status, TaskState};
use anyhow::{bail, Context, Result};
use std::path::Path;

/// `Ok(())` lets `worktree remove` delete `wt`; `Err` refuses (non-zero exit,
/// nothing removed). See the module docs.
pub fn refuse_unverified_teardown(cfg: &Config, cwd: &Path, repo: &Path, wt: &Path) -> Result<()> {
    let shown = wt.display();
    let undetermined = |why: String| -> anyhow::Error {
        anyhow::anyhow!(
            "refusing to remove worktree {shown}: cannot determine whether the task that owns \
             it is verified ({why}); its tdd proofs are what the fail-to-pass oracle reads. \
             Verify the task first, or reset it with `condukt state abandon --run <run> --task \
             <id>`."
        )
    };
    let branch = match checked_out_branch(wt) {
        Ok(b) => b,
        Err(e) => return Err(undetermined(format!("its checked-out branch: {e:#}"))),
    };
    let identity = match branch.as_deref() {
        Some(b) => crate::worktree::recorded_task_identity(repo, b)
            .map_err(|e| undetermined(format!("task identity of branch '{b}': {e:#}")))?,
        None => None,
    };
    if let Some(id) = identity {
        let Some((run, task)) = id
            .split_once('/')
            .filter(|(r, t)| !r.is_empty() && !t.is_empty())
        else {
            return Err(undetermined(format!(
                "malformed task identity {id:?} (expected <run>/<task>)"
            )));
        };
        let rs = RunState::load(cfg, cwd, run)
            .map_err(|e| undetermined(format!("run '{run}' state: {e:#}")))?;
        let Some(t) = rs.tasks.iter().find(|t| t.id == task) else {
            return Err(undetermined(format!(
                "branch identity names task '{task}' of run '{run}', which that run does not have"
            )));
        };
        return judge(run, t, wt);
    }
    let runs = runs_strict(cfg, cwd).map_err(|e| undetermined(format!("{e:#}")))?;
    for rs in &runs {
        for t in &rs.tasks {
            if t.worktree
                .as_deref()
                .is_some_and(|p| same_path(Path::new(p), wt))
            {
                judge(&rs.run_id, t, wt)?;
            }
        }
    }
    Ok(())
}

/// Whether `t` (owner of `wt`) permits the teardown.
fn judge(run: &str, t: &TaskState, wt: &Path) -> Result<()> {
    let ok = match t.status {
        Status::Verified | Status::Cancelled | Status::Discarded => true,
        Status::Pending => t.worktree.is_none(),
        Status::Running | Status::Done | Status::Failed => false,
    };
    if ok {
        return Ok(());
    }
    bail!(
        "refusing to remove worktree {}: task '{}' of run '{run}' has status {} (not verified); \
         removing it would delete the tdd proofs the fail-to-pass oracle reads. Verify the task \
         first (`condukt state set --run {run} --task {} --status verified`), or, to give the \
         task up and re-dispatch it, reset it with `condukt state abandon --run {run} --task {}` \
         (or `condukt state cancel`) and then remove the worktree.",
        wt.display(),
        t.id,
        format!("{:?}", t.status).to_lowercase(),
        t.id,
        t.id
    )
}

/// The branch checked out in `wt`: `Ok(None)` = detached HEAD (git answered
/// "not a symbolic ref"); any other git outcome is an `Err`.
fn checked_out_branch(wt: &Path) -> Result<Option<String>> {
    let args = ["symbolic-ref", "-q", "--short", "HEAD"];
    match crate::worktree::git_exit_code(wt, &args)? {
        0 => {
            let b = crate::worktree::git(wt, &args)?;
            let b = b.trim();
            if b.is_empty() {
                bail!("git symbolic-ref printed no branch name");
            }
            Ok(Some(b.to_string()))
        }
        1 => Ok(None),
        code => bail!("git symbolic-ref HEAD exited {code}"),
    }
}

/// Every run state of the project; unlike [`state::all_runs`] (best-effort
/// discovery) any unreadable dir, entry or state file is an `Err`.
fn runs_strict(cfg: &Config, cwd: &Path) -> Result<Vec<RunState>> {
    let mut runs = Vec::new();
    for dir in state::project_state_dirs(cfg, cwd)? {
        let rd = match std::fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                return Err(e).with_context(|| format!("reading state dir {}", dir.display()))
            }
        };
        for entry in rd {
            let entry = entry.with_context(|| format!("reading an entry of {}", dir.display()))?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let fname = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if state::is_run_state_sidecar(fname) {
                continue;
            }
            let txt = std::fs::read_to_string(&path)
                .with_context(|| format!("reading run state {}", path.display()))?;
            let rs: RunState = serde_json::from_str(&txt)
                .with_context(|| format!("parsing run state {}", path.display()))?;
            runs.push(rs);
        }
    }
    Ok(runs)
}

/// `a` and `b` name the same directory: equal as given, or equal once both
/// canonicalize.
fn same_path(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    matches!(
        (std::fs::canonicalize(a), std::fs::canonicalize(b)),
        (Ok(x), Ok(y)) if x == y
    )
}
