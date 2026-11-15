//! `backlog recheck [--id ID]` (user ruling 2026-11-13:
//! 「保留はありえない実証されないならclose,実証されたらopen」).
//!
//! Re-runs the stored repro of every OPEN row (`pending`, `failed`, and legacy
//! `ruled-deferred` rows written by the abolished `backlog defer`) that is not
//! under a live claim lease, and resolves each one — there is no third,
//! "kept for later" state:
//!   - `reproduced` -> stays open. A legacy `ruled-deferred` row becomes
//!     `pending` (the deferral state no longer exists).
//!   - `not-reproduced` / `not-demonstrated` / no recorded repro at all ->
//!     AUTO-CANCELLED (discard closure recording the observation verbatim;
//!     mirrored issue closed like `backlog cancel`).
//!   - `undeterminable` (infrastructure: timeout, signal, build failure, git
//!     failure, dirty checkout ...) -> the row is left untouched and named on
//!     stderr; after the remaining rows are processed the command exits
//!     non-zero (CLAUDE.md §3: undetermined is not clean).
//!
//! A store write: the caller resolves the store through the primary-tree
//! guard (linked worktree only). One stdout line per row:
//! `<verdict>\t<action>\t<id>`.

use anyhow::{anyhow, Result};
use std::collections::HashSet;
use std::path::Path;

use crate::claim_recheck::{auto_cancel, rerun_dir, NO_DEFERRAL_RULING};
use crate::evidence::{self, RecheckVerdict};
use crate::store::{self, STATUS_RULED_DEFERRED};
use crate::task::{Task, STATUS_FAILED, STATUS_PENDING};

/// Arguments of `backlog recheck`.
#[derive(clap::Args, Debug)]
pub struct RecheckArgs {
    /// Only this task (it must be open: pending / failed / ruled-deferred)
    #[arg(long)]
    pub id: Option<String>,
}

fn is_open(t: &Task) -> bool {
    t.status == STATUS_PENDING || t.status == STATUS_FAILED || t.status == STATUS_RULED_DEFERRED
}

/// The `recheck` body. `leased` = ids under a live claim lease (skipped: a
/// worker holds them). `store_root` = the repo holding the store (`None` for a
/// pinned store): in a repo store the store's repo is every row's identity
/// (user ruling 2026-11-14), see `claim_recheck::rerun_dir`. `Err` (non-zero)
/// when any row was undeterminable, when `--id` names no open row, or when a
/// write failed.
pub fn run(
    tasks_path: &Path,
    args: &RecheckArgs,
    leased: &HashSet<String>,
    store_root: Option<&Path>,
) -> Result<()> {
    let tasks = store::load(tasks_path)?;
    let rows: Vec<Task> = match &args.id {
        Some(id) => {
            let t = tasks
                .iter()
                .find(|t| &t.id == id)
                .ok_or_else(|| anyhow!("task not found: {id}"))?;
            if !is_open(t) {
                return Err(anyhow!(
                    "refused: task {id} is {} — only open rows (pending / failed / \
                     ruled-deferred) are rechecked",
                    t.status
                ));
            }
            vec![t.clone()]
        }
        None => tasks.iter().filter(|t| is_open(t)).cloned().collect(),
    };
    let mut undeterminable: Vec<String> = Vec::new();
    for t in rows {
        if leased.contains(&t.id) {
            println!("leased\tskipped\t{}", t.id);
            eprintln!(
                "backlog recheck: {} is under a live claim lease (a worker holds it); not \
                 rechecked",
                t.id
            );
            continue;
        }
        let Some(stored) = t.repro.as_ref() else {
            let reason = format!(
                "auto-cancel by `backlog recheck` (user ruling 2026-11-13 {NO_DEFERRAL_RULING}): \
                 the row records NO repro, so nothing demonstrates the defect"
            );
            store::mark_cancelled(tasks_path, &t.id, &reason)?;
            crate::mirror_close_for(tasks_path, &t.id);
            println!("no-repro\tcancelled\t{}", t.id);
            continue;
        };
        let root = match rerun_dir(&t, store_root) {
            Ok(r) => r,
            Err(why) => {
                println!("undeterminable\tuntouched\t{}", t.id);
                eprintln!(
                    "backlog recheck: {} NOT resolved (undeterminable, row untouched): where to \
                     re-run its stored repro could not be determined (cmd: {}): {why}",
                    t.id, stored.cmd
                );
                undeterminable.push(t.id.clone());
                continue;
            }
        };
        let c = evidence::run_repro_classified(&root, &stored.cmd, store::now_unix());
        match c.verdict {
            RecheckVerdict::Reproduced => {
                let action = if t.status == STATUS_RULED_DEFERRED {
                    reopen_ruled_deferred(tasks_path, &t.id)?;
                    "reopened-pending"
                } else {
                    "kept-open"
                };
                println!("{}\t{action}\t{}", c.verdict.label(), t.id);
            }
            RecheckVerdict::NotReproduced | RecheckVerdict::NotDemonstrated => {
                auto_cancel(tasks_path, &t.id, &c, "recheck")?;
                println!("{}\tcancelled\t{}", c.verdict.label(), t.id);
                eprintln!(
                    "backlog recheck: AUTO-CANCELLED {} ({}; cmd: {}; rev: {}): {}",
                    t.id,
                    c.verdict.label(),
                    c.repro.cmd,
                    c.repro.rev.as_deref().unwrap_or("<unresolved>"),
                    c.repro.detail
                );
            }
            RecheckVerdict::Undeterminable => {
                println!("{}\tuntouched\t{}", c.verdict.label(), t.id);
                eprintln!(
                    "backlog recheck: {} NOT resolved (undeterminable, row untouched; cmd: {}; \
                     rev: {}): {}",
                    t.id,
                    c.repro.cmd,
                    c.repro.rev.as_deref().unwrap_or("<unresolved>"),
                    c.repro.detail
                );
                undeterminable.push(t.id.clone());
            }
        }
    }
    if undeterminable.is_empty() {
        Ok(())
    } else {
        Err(anyhow!(
            "backlog recheck: {} row(s) could not be determined and were left untouched (this is \
             NOT \"all clean\"): {}",
            undeterminable.len(),
            undeterminable.join(", ")
        ))
    }
}

/// A legacy `ruled-deferred` row whose repro still reproduces goes back to
/// `pending`: the deferral state no longer exists.
fn reopen_ruled_deferred(tasks_path: &Path, id: &str) -> Result<()> {
    store::with_tasks_lock_required(tasks_path, "recheck", || {
        let mut tasks = store::load(tasks_path)?;
        let task = tasks
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or_else(|| anyhow!("task not found: {id}"))?;
        if task.status != STATUS_RULED_DEFERRED {
            return Ok(());
        }
        task.status = STATUS_PENDING;
        let line = format!(
            "recheck: reopened as pending (the deferral state is abolished, user ruling \
             2026-11-13 {NO_DEFERRAL_RULING}; its stored repro still reproduces)"
        );
        if task.notes.is_empty() {
            task.notes = line;
        } else {
            task.notes.push('\n');
            task.notes.push_str(&line);
        }
        store::touch(task, store::now_unix());
        store::save(tasks_path, &tasks)
    })
}
