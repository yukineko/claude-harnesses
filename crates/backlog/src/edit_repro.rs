//! `backlog edit`: field edits, plus `--repro-test CMD`, which replaces a
//! pending task's stored repro.
//!
//! Why `--repro-test` exists: an intentional RED repro is moved under
//! `#[ignore = "backlog <id>: ..."]`, so the stored repro must change to
//! `cargo test ... -- --ignored`. Hand-editing `.backlog/tasks.toml` would
//! skip backlog's evidence check. `edit --repro-test` runs the new command
//! through the SAME path `backlog add --repro-test` uses
//! ([`crate::evidence::run_repro`] at HEAD in a temp worktree, then
//! [`Intake::reproduced`] as the filter). Only a `reproduced` outcome is
//! stored, with the full [`Repro`] record `add` stores. `not-reproduced` and
//! `undetermined` (including a run that executed zero tests) are refused with
//! a non-zero exit, and nothing is written.
//!
//! Atomicity: the repro runs BEFORE the store lock is taken (a test can run
//! for up to `BACKLOG_TEST_TIMEOUT_SECS`). The other flags (`--title`,
//! `--tag`, `--notes`, `--status`) are applied in the SAME locked write as the
//! new repro. When the repro is refused, none of them is written.
//!
//! Only a `pending` task's repro can be replaced. Any other status is refused
//! before the command runs and checked again under the lock, because the
//! status can change while the test runs.
//!
//! The `edit` arm in `main.rs` does not close the GitHub mirror, and does not
//! need to. A terminal transition mirrors its close in whichever verb performs
//! it. `edit` is not such a verb: it refuses every `--status` except
//! `pending` / `failed`, with nothing written, so `edit --status
//! done|cancelled` cannot finish work locally while the issue stays open. The
//! verbs that CAN reach a terminal state (`done`, `cancel`) each mirror inline.
//! Pinned by tests/mirror_gaps_after_0323.rs
//! (`edit_status_terminal_is_refused_and_leaves_store_and_issue_untouched`).
use anyhow::{anyhow, Result};
use std::path::Path;

use crate::store::Intake;
use crate::store::{is_terminal_status, load, now_unix, save, touch, with_tasks_lock_required};
use crate::task::{Repro, Status, Task, STATUS_FAILED, STATUS_PENDING};

/// The fields `backlog edit` can change. `None` (or `tags: None`) leaves the
/// field as it is.
pub struct Fields<'a> {
    pub title: Option<&'a str>,
    pub tags: Option<Vec<String>>,
    pub notes: Option<&'a str>,
    pub status: Option<&'a str>,
}

/// `backlog edit ID [field flags] [--repro-test CMD]`.
///
/// Without `repro_test` this is a plain field edit. With it, the command is run
/// as `backlog add --repro-test` runs it (from the current directory), and only
/// a `reproduced` outcome lets the edit, including every field flag, be
/// written.
pub fn edit_with_repro_test(
    path: &Path,
    id: &str,
    fields: Fields<'_>,
    repro_test: Option<&str>,
) -> Result<()> {
    let Some(cmd) = repro_test else {
        return apply(path, id, fields, None);
    };
    // Refuse what can be refused without running a test.
    parse_edit_status(fields.status)?;
    let tasks = load(path)?;
    require_pending(find(&tasks, id)?)?;
    let repro = crate::evidence::run_repro(&std::env::current_dir()?, cmd, now_unix());
    let why = format!("{}: {}", repro.outcome, repro.detail);
    let Some(intake) = Intake::reproduced(repro) else {
        return Err(anyhow!(
            "backlog edit --repro-test: REFUSED, nothing written (repro outcome {why}). The \
             stored repro is replaced only when CMD FAILS behaviourally at HEAD, as \
             `backlog add --repro-test` requires; for an #[ignore]d RED test pass `-- --ignored`"
        ));
    };
    apply(path, id, fields, intake.repro)?;
    eprintln!("repro: {why}; stored as the repro of {id}");
    Ok(())
}

/// フィールドの一部を更新して保存。None のフィールドは変更しない。
/// (Test-only signature kept for the existing store tests; `main.rs` calls
/// [`edit_with_repro_test`].)
///
/// CA-backlog-004: an unknown `status` is REJECTED (validated against the
/// stored [`crate::task::STATUSES`] vocabulary) BEFORE any write.
/// Previously `edit --status open` wrote the raw typo through, stranding the
/// task out of `next`/`next_claim` (neither `open` nor any non-vocabulary
/// value is `is_pending()`) with no path back — `requeue_expired` only rescues
/// deferred/stale-claimed tasks, so the typo'd task was lost. Rejecting up
/// front keeps the task in its last valid state.
#[cfg(test)]
pub fn edit(
    path: &Path,
    id: &str,
    title: Option<&str>,
    tags: Option<Vec<String>>,
    notes: Option<&str>,
    status: Option<&str>,
) -> Result<()> {
    let fields = Fields {
        title,
        tags,
        notes,
        status,
    };
    apply(path, id, fields, None)
}

/// The single locked write behind every `backlog edit`.
/// Every refusal happens before any field is touched, so a refused edit leaves
/// the task exactly as it was.
fn apply(path: &Path, id: &str, fields: Fields<'_>, repro: Option<Repro>) -> Result<()> {
    with_tasks_lock_required(path, "edit", || {
        let status = parse_edit_status(fields.status)?;
        let mut tasks = load(path)?;
        let task = tasks
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or_else(|| anyhow!("task not found: {}", id))?;
        // backlog 45c3a699: terminal is monotonic. Refuse BEFORE touching any
        // field so a refused edit leaves the task exactly as it was.
        if let Some(v) = status {
            if is_terminal_status(&task.status) && !is_terminal_status(&v) {
                return Err(anyhow!(
                    "refused: task {id} is {} (terminal) and terminal status is final; \
                     `edit --status {v}` would reopen it. File a new task for the remaining work",
                    task.status
                ));
            }
            // backlog b3f5e3af: a ruled-deferred row is off the queue by a
            // recorded user ruling; `edit --status pending|failed` would hand it
            // out again with the ruling still in its notes.
            if task.status == Status::RuledDeferred {
                return Err(anyhow!(
                    "refused: task {id} is ruled-deferred (a legacy row of the abolished \
                     deferral state, user ruling 2026-11-13); `edit --status {v}` would put it \
                     back on the queue without re-observing it. Resolve it with `backlog \
                     recheck --id {id}` (reproduced -> pending, otherwise auto-cancelled) or \
                     `backlog cancel {id} --reason R`"
                ));
            }
        }
        // Re-checked under the lock: the status may have changed while the
        // repro ran.
        if repro.is_some() {
            require_pending(task)?;
        }
        if let Some(v) = fields.title {
            task.title = v.to_string();
        }
        if let Some(v) = fields.tags {
            task.tags = v;
        }
        if let Some(v) = fields.notes {
            task.notes = v.to_string();
        }
        if let Some(v) = status {
            task.status = v;
        }
        if let Some(r) = repro {
            task.repro = Some(r);
        }
        touch(task, now_unix());
        save(path, &tasks)
    })
}

/// CA-backlog-004: reject an unknown --status up front so a typo can never be
/// persisted and strand the task. Validated against the STORED vocabulary, so
/// the derived `claimed` is refused too (0dafa254).
///
/// Close-evidence: `edit --status` is not a route to any status that needs a
/// recorded closure. Terminal statuses need one (`backlog done ...
/// --test/--doc-only/--duplicate-of`, `backlog cancel --reason`'s discard).
fn parse_edit_status(status: Option<&str>) -> Result<Option<Status>> {
    if status == Some(Status::RuledDeferred.as_str()) {
        return Err(anyhow!(
            "refused: `edit --status ruled-deferred` is not allowed; the deferral state is \
             abolished (user ruling 2026-11-13: an open row stays open only while its repro \
             reproduces — `backlog recheck`)"
        ));
    }
    if let Some(w) = crate::task::stored_status_error(status) {
        return Err(anyhow!("{w}"));
    }
    let status: Option<Status> = match status.map(str::parse::<Status>).transpose() {
        Ok(v) => v,
        Err(e) => return Err(anyhow!("{e}")),
    };
    if let Some(v) = status {
        if v != STATUS_PENDING && v != STATUS_FAILED {
            return Err(anyhow!(
                "refused: `edit --status {v}` is not allowed. `done` needs evidence: \
                 `backlog done ID --test CMD --red-rev REV`, `--doc-only COMMIT` or \
                 `--duplicate-of ID`; `cancelled` is `backlog cancel ID --reason R` (a \
                 recorded discard). There is no evidence-free close: an item no test \
                 reproduces is cancelled"
            ));
        }
    }
    Ok(status)
}

fn find<'t>(tasks: &'t [Task], id: &str) -> Result<&'t Task> {
    tasks
        .iter()
        .find(|t| t.id == id)
        .ok_or_else(|| anyhow!("task not found: {}", id))
}

fn require_pending(task: &Task) -> Result<()> {
    if task.status == STATUS_PENDING {
        return Ok(());
    }
    Err(anyhow!(
        "refused: task {} is {}; `edit --repro-test` replaces the repro of a pending task only, \
         nothing written",
        task.id,
        task.status
    ))
}
