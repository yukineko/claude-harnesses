//! `backlog defer ID --ruling QUOTE` (backlog b3f5e3af): record a user ruling
//! (見送り) as the stored status `ruled-deferred`, which `next` /
//! `next --claim` never hand out and nothing requeues.

use anyhow::{anyhow, Result};
use std::path::Path;

use crate::store::{
    is_terminal_status, load, now_unix, save, touch, with_tasks_lock_required,
    STATUS_RULED_DEFERRED,
};

/// Arguments of `backlog defer`.
#[derive(clap::Args, Debug)]
pub struct DeferArgs {
    /// Task ID
    pub id: String,

    /// The user's ruling, quoted (required, non-empty; appended to the
    /// notes as `ruling (defer): <quote>`)
    #[arg(long)]
    pub ruling: String,
}

/// The `defer` subcommand body: record the ruling in the store at
/// `tasks_path` and print `ruled-deferred: <id>`. Every refusal is an `Err`
/// (non-zero exit), never a silent success.
pub fn run(tasks_path: &Path, args: &DeferArgs) -> Result<()> {
    mark_ruled_deferred(tasks_path, &args.id, &args.ruling)?;
    println!("ruled-deferred: {}", args.id);
    Ok(())
}

/// Mark a task `ruled-deferred` with a recorded user ruling (backlog b3f5e3af;
/// user ruling 2026-11-07 「実装で補えるものは実装にして。LLMの推論での防御は
/// すべてあてにならない」). A ruling that lived only in the notes depended on
/// every dispatcher remembering to grep for it; this status makes the
/// exclusion CODE: `next` / `next --claim` select only `pending` / `failed`
/// ([`crate::task::Task::is_pending`]), `requeue_expired` moves only those two,
/// `fail` and `edit --status` refuse to move this row back onto the queue, and
/// `add` refuses to re-file the same content.
///
/// `ruling` is required and non-empty (a deferral with no quoted ruling
/// records nothing a reviewer can check); it is appended to the notes as
/// `ruling (defer): <quote>`. Any `defer_until` is cleared — this deferral is
/// untimed and nothing lifts it automatically.
///
/// Refused, with nothing written: an unknown id, a terminal (`done` /
/// `cancelled`) task. Idempotent for an already-ruled-deferred task whose
/// notes already carry this exact ruling line; a DIFFERENT ruling on such a
/// task is appended (it is a new record).
pub fn mark_ruled_deferred(path: &Path, id: &str, ruling: &str) -> Result<()> {
    let ruling = ruling.trim();
    if ruling.is_empty() {
        return Err(anyhow!(
            "refused: `defer {id}` needs a non-empty --ruling (the user's ruling, quoted; it \
             is recorded in the notes)"
        ));
    }
    let line = format!("ruling (defer): {ruling}");
    with_tasks_lock_required(path, "defer", || {
        let mut tasks = load(path)?;
        let task = tasks
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or_else(|| anyhow!("task not found: {}", id))?;
        if is_terminal_status(&task.status) {
            return Err(anyhow!(
                "refused: task {id} is {} (terminal); there is nothing left to defer",
                task.status
            ));
        }
        if task.status == STATUS_RULED_DEFERRED && task.notes.lines().any(|l| l == line) {
            return Ok(());
        }
        task.status = STATUS_RULED_DEFERRED;
        if task.notes.is_empty() {
            task.notes = line.clone();
        } else {
            task.notes.push('\n');
            task.notes.push_str(&line);
        }
        task.defer_until = None;
        touch(task, now_unix());
        save(path, &tasks)
    })
}
