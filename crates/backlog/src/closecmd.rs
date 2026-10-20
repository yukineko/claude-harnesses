//! The evidence-gated terminal transitions and the findings intake:
//! `done` (F2P test / doc-only commit / duplicate) and the read-only
//! `audit-closures` (close-evidence spec, user-ratified 2026-10-01).
//! There is no evidence-free close: the `ruling` subcommand and its
//! `needs-ruling` status were removed (user ruling 2026-10-15); an item no
//! test reproduces is cancelled (`backlog cancel ID --reason R`).
//!
//! Every path that cannot determine its answer REFUSES with a named cause
//! (non-zero exit, nothing written). Nothing here turns "could not observe"
//! into "closed".

use crate::evidence;
use crate::store;
use crate::task::{Closure, Task, REPRO_REPRODUCED, STATUS_DONE, STATUS_PENDING};
use anyhow::{anyhow, bail, Result};
use std::path::Path;

/// The closure reasons `done --test` accepts.
const TEST_REASONS: [&str; 3] = ["fixed", "already-fixed", "obsolete"];

fn now_unix() -> Result<i64> {
    let d = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| anyhow!("system clock is before the unix epoch: {e}"))?;
    i64::try_from(d.as_secs()).map_err(|e| anyhow!("system clock out of range: {e}"))
}

fn find<'a>(tasks: &'a [Task], id: &str) -> Result<&'a Task> {
    tasks
        .iter()
        .find(|t| t.id == id)
        .ok_or_else(|| anyhow!("task not found: {id}"))
}

fn toplevel() -> Result<std::path::PathBuf> {
    let cwd = std::env::current_dir()?;
    evidence::repo_toplevel(&cwd).map_err(|e| anyhow!("refused: {e}"))
}

/// Arguments of `backlog done`.
pub struct DoneArgs {
    pub id: String,
    pub test: Option<String>,
    pub red_rev: Option<String>,
    /// `--red-overlay PATH` (repeatable): narrow the RED overlay to these
    /// HEAD test files. Empty = the default overlay.
    pub red_overlay: Vec<String>,
    pub reason: Option<String>,
    pub duplicate_of: Option<String>,
    pub doc_only: Option<String>,
}

const DONE_USAGE: &str = "`backlog done ID --test CMD --red-rev REV [--reason \
     fixed|already-fixed|obsolete]` (an executed committed test, RED at REV and GREEN at HEAD), \
     `backlog done ID --doc-only COMMIT`, or `backlog done ID --duplicate-of ID`. There is no evidence-free close: \
     an item no test reproduces is discarded with `backlog cancel ID --reason R`";

/// `backlog done`: close with recorded evidence, or refuse naming why.
pub fn done(tasks_path: &Path, a: DoneArgs) -> Result<()> {
    let routes = usize::from(a.test.is_some())
        + usize::from(a.duplicate_of.is_some())
        + usize::from(a.doc_only.is_some());
    if routes == 0 {
        bail!(
            "refused: `backlog done {}` without evidence. Suspicion is never evidence; use {DONE_USAGE}",
            a.id
        );
    }
    if routes > 1 {
        bail!("refused: give exactly one of --test, --doc-only, --duplicate-of");
    }
    if a.test.is_none() && (a.red_rev.is_some() || a.reason.is_some() || !a.red_overlay.is_empty())
    {
        bail!(
            "refused: --red-rev / --red-overlay / --reason belong to a --test close (an obsolete close also needs \
             a committed test proving the old surface is gone); use {DONE_USAGE}"
        );
    }

    // Cheap status check BEFORE any test runs; re-checked under the lock.
    let tasks = store::load(tasks_path)?;
    let current = find(&tasks, &a.id)?;
    // CA-backlog-003 idempotency, kept for at-least-once callers (a retried
    // `done` after a timeout): an evidence-bearing `done` on a row that is
    // ALREADY `done` is a no-op success. Nothing is run or recorded, and the
    // message says so — it does not claim this call's evidence was observed.
    // A bare `done` never gets here (refused above).
    if current.status == STATUS_DONE {
        println!(
            "already done: {} (terminal; nothing was run or recorded by this call)",
            a.id
        );
        return Ok(());
    }
    store::require_closable(current)?;
    let now = now_unix()?;

    let closure = if let Some(test) = &a.test {
        let reason = a.reason.clone().unwrap_or_else(|| "fixed".to_string());
        if !TEST_REASONS.contains(&reason.as_str()) {
            bail!(
                "refused: unknown --reason {reason:?}; valid: {}",
                TEST_REASONS.join(" | ")
            );
        }
        let Some(red_rev) = &a.red_rev else {
            bail!(
                "refused: --red-rev is required. F2P is required for every test close (fixed, \
                 already-fixed and obsolete alike): the test must FAIL behaviourally at --red-rev \
                 and PASS at HEAD"
            );
        };
        let cmd = evidence::parse_test_cmd(test).map_err(|e| anyhow!("refused: {e}"))?;
        let root = toplevel()?;
        let overlay = (!a.red_overlay.is_empty()).then_some(a.red_overlay.as_slice());
        let f2p = evidence::run_f2p(&root, &cmd, red_rev, overlay, now)
            .map_err(|e| anyhow!("refused: {e}"))?;
        Closure {
            reason,
            duplicate_of: None,
            doc_only_commit: None,
            green: Some(f2p.green),
            red: Some(f2p.red),
            discard_reason: None,
        }
    } else if let Some(commit) = &a.doc_only {
        let root = toplevel()?;
        let c = evidence::verify_doc_only(&root, commit).map_err(|e| anyhow!("refused: {e}"))?;
        Closure {
            reason: "doc-only".to_string(),
            duplicate_of: None,
            doc_only_commit: Some(c),
            green: None,
            red: None,
            discard_reason: None,
        }
    } else if let Some(target) = &a.duplicate_of {
        Closure {
            reason: "duplicate".to_string(),
            duplicate_of: Some(target.clone()),
            doc_only_commit: None,
            green: None,
            red: None,
            discard_reason: None,
        }
    } else {
        bail!("refused: no evidence route given; use {DONE_USAGE}");
    };

    let dup = a.duplicate_of.clone();
    store::update_task(tasks_path, "done", &a.id, |task, all| {
        if let Some(target) = &dup {
            check_duplicate_target(&task.id, target, all)?;
        }
        store::apply_closure(task, STATUS_DONE, closure)
    })?;
    println!("done: {}", a.id);
    Ok(())
}

/// A duplicate target must exist, not be the task itself, and have stored
/// status `pending` (which includes a leased row shown as `claimed`) or
/// `done` — including a legacy `done` row with no closure table (user ruling
/// 2026-10-01, matching the pre-commit gate's case P8). Every other status
/// (`failed`, `cancelled`) is refused.
fn check_duplicate_target(id: &str, target: &str, all: &[Task]) -> Result<()> {
    if target == id {
        bail!("refused: task {id} cannot be a duplicate of itself");
    }
    let Some(t) = all.iter().find(|t| t.id == target) else {
        bail!("refused: --duplicate-of {target}: no such task in this store");
    };
    match t.status {
        STATUS_PENDING | STATUS_DONE => Ok(()),
        s => bail!(
            "refused: --duplicate-of {target} has status {s}; the target must be pending \
             (or claimed) or done"
        ),
    }
}

// ---- audit-closures ----------------------------------------------------------

/// Classes of a terminal row's closure.
fn classify(t: &Task) -> &'static str {
    if let Some(c) = &t.closure {
        if c.green.is_some() && c.red.is_some() {
            return "observed-f2p";
        }
        if c.doc_only_commit.is_some() {
            return "doc-only";
        }
        if c.duplicate_of.is_some() {
            return "duplicate";
        }
        if c.discard_reason.is_some() {
            return "discard";
        }
    }
    let notes = t.notes.trim();
    if notes.is_empty() {
        "none"
    } else if cites(notes) {
        "cited-only"
    } else {
        "judgment"
    }
}

/// Does `notes` cite a commit (7..40 hex chars) or a `path:line`? A citation
/// is recorded as such — it is never evidence.
fn cites(notes: &str) -> bool {
    notes
        .split(|c: char| c.is_whitespace() || c == ',' || c == '(' || c == ')')
        .any(|w| {
            let w = w.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != ':' && c != '/');
            let hex = w.len() >= 7
                && w.len() <= 40
                && w.chars().all(|c| c.is_ascii_hexdigit())
                && w.chars().any(|c| c.is_ascii_digit());
            let path_line = match w.rsplit_once(':') {
                Some((p, l)) => {
                    !p.is_empty()
                        && (p.contains('/') || p.contains('.'))
                        && !l.is_empty()
                        && l.chars().all(|c| c.is_ascii_digit())
                }
                None => false,
            };
            hex || path_line
        })
}

/// `audit-closures [--json]`: read-only. Load failure → `Err` (non-zero).
pub fn audit_closures(tasks_path: &Path, as_json: bool) -> Result<()> {
    let tasks = store::load(tasks_path)?;
    let terminal: Vec<&Task> = tasks
        .iter()
        .filter(|t| store::is_terminal_status(&t.status))
        .collect();
    let mut counts: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    let rows: Vec<serde_json::Value> = terminal
        .iter()
        .map(|t| {
            let class = classify(t);
            *counts.entry(class).or_insert(0) += 1;
            serde_json::json!({
                "id": t.id,
                "title": t.title,
                "status": t.status,
                "class": class,
            })
        })
        .collect();
    if as_json {
        let v = serde_json::json!({
            "rows": rows,
            "counts": counts,
        });
        println!("{}", serde_json::to_string(&v)?);
        return Ok(());
    }
    println!(
        "closure audit: {} terminal row(s) (read-only; nothing is reopened)",
        terminal.len()
    );
    for (class, n) in &counts {
        println!("  {class:<16} {n}");
    }
    for t in &terminal {
        println!("{:<10} {:<16} {}", t.id, classify(t), t.title);
    }
    Ok(())
}

/// The evidence label `list` shows for a row.
pub fn evidence_label(t: &Task) -> String {
    if let Some(c) = &t.closure {
        return format!("closed:{}", classify_closure_label(c));
    }
    match &t.repro {
        Some(r) if r.outcome == REPRO_REPRODUCED => "observed".to_string(),
        Some(r) => format!("repro:{}", r.outcome),
        None => "legacy".to_string(),
    }
}

fn classify_closure_label(c: &Closure) -> &str {
    if c.green.is_some() {
        "f2p"
    } else if c.doc_only_commit.is_some() {
        "doc-only"
    } else if c.duplicate_of.is_some() {
        "duplicate"
    } else if c.discard_reason.is_some() {
        "discard"
    } else {
        "none"
    }
}
