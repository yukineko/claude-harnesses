//! The evidence-gated terminal transitions and the findings intake:
//! `done` (F2P test / doc-only commit / duplicate), `ruling
//! request|approve|withdraw|list`, `confirm`, and the read-only
//! `audit-closures` (close-evidence spec, user-ratified 2026-10-01).
//!
//! Every path that cannot determine its answer REFUSES with a named cause
//! (non-zero exit, nothing written). Nothing here turns "could not observe"
//! into "closed".

use crate::evidence;
use crate::store;
use crate::task::{
    Closure, RulingRecord, Task, REPRO_REPRODUCED, STATUS_DONE, STATUS_FAILED, STATUS_NEEDS_RULING,
    STATUS_PENDING, STATUS_UNCONFIRMED,
};
use anyhow::{anyhow, bail, Result};
use std::io::{BufRead, IsTerminal, Write};
use std::path::Path;

/// The closure reasons `done --test` accepts.
const TEST_REASONS: [&str; 3] = ["fixed", "already-fixed", "obsolete"];

/// Ruling kinds.
pub const RULING_JUDGMENT: &str = "judgment";
pub const RULING_UNTESTABLE: &str = "untestable";

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
    pub reason: Option<String>,
    pub duplicate_of: Option<String>,
    pub doc_only: Option<String>,
}

const DONE_USAGE: &str = "`backlog done ID --test CMD --red-rev REV [--reason \
     fixed|already-fixed|obsolete]` (an executed committed test, RED at REV and GREEN at HEAD), \
     `backlog done ID --doc-only COMMIT`, or `backlog done ID --duplicate-of ID`. A judgment or \
     untestable close goes through `backlog ruling request` and a human `ruling approve`";

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
    if a.test.is_none() && (a.red_rev.is_some() || a.reason.is_some()) {
        bail!(
            "refused: --red-rev / --reason belong to a --test close (an obsolete close also needs \
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
        let f2p =
            evidence::run_f2p(&root, &cmd, red_rev, now).map_err(|e| anyhow!("refused: {e}"))?;
        Closure {
            reason,
            duplicate_of: None,
            doc_only_commit: None,
            green: Some(f2p.green),
            red: Some(f2p.red),
            ruling: None,
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
            ruling: None,
        }
    } else if let Some(target) = &a.duplicate_of {
        Closure {
            reason: "duplicate".to_string(),
            duplicate_of: Some(target.clone()),
            doc_only_commit: None,
            green: None,
            red: None,
            ruling: None,
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
/// (`failed`, `cancelled`, `unconfirmed`, `needs-ruling`) is refused.
fn check_duplicate_target(id: &str, target: &str, all: &[Task]) -> Result<()> {
    if target == id {
        bail!("refused: task {id} cannot be a duplicate of itself");
    }
    let Some(t) = all.iter().find(|t| t.id == target) else {
        bail!("refused: --duplicate-of {target}: no such task in this store");
    };
    match t.status.as_str() {
        STATUS_PENDING | STATUS_DONE => Ok(()),
        s => bail!(
            "refused: --duplicate-of {target} has status {s}; the target must be pending \
             (or claimed) or done"
        ),
    }
}

// ---- ruling ------------------------------------------------------------------

/// `ruling request`.
pub fn ruling_request(
    tasks_path: &Path,
    id: &str,
    kind: &str,
    rationale: Option<String>,
    untestable_reason: Option<String>,
) -> Result<()> {
    let nonempty = |v: &Option<String>| v.as_deref().is_some_and(|s| !s.trim().is_empty());
    match kind {
        RULING_JUDGMENT => {
            if !nonempty(&rationale) {
                bail!("refused: a judgment ruling needs --rationale");
            }
        }
        RULING_UNTESTABLE => {
            if !nonempty(&untestable_reason) {
                bail!(
                    "refused: an untestable ruling needs --untestable-reason. First try to make it \
                     testable (a seam, fault injection, a deterministic race repro, a sandboxed \
                     deployed-binary probe); only then `condukt policy answer --untestable` and \
                     this request"
                );
            }
        }
        other => bail!("refused: unknown ruling kind {other:?}; valid: judgment | untestable"),
    }
    store::update_task(tasks_path, "ruling request", id, |task, _| {
        match task.status.as_str() {
            STATUS_PENDING | STATUS_FAILED | STATUS_UNCONFIRMED => {}
            s => bail!(
                "refused: task {id} is {s}; a ruling can be requested only for a pending, failed \
                 or unconfirmed task"
            ),
        }
        task.status = STATUS_NEEDS_RULING.to_string();
        task.ruling_kind = Some(kind.to_string());
        task.rationale = rationale;
        task.untestable_reason = untestable_reason;
        task.defer_until = None;
        Ok(())
    })?;
    println!("needs-ruling: {id} ({kind}); a human approves it with `backlog ruling approve {id}`");
    Ok(())
}

/// Who is approving, from the login environment. Undeterminable → refuse.
fn approver() -> Result<String> {
    for var in ["USER", "LOGNAME"] {
        if let Ok(v) = std::env::var(var) {
            if !v.trim().is_empty() {
                return Ok(v.trim().to_string());
            }
        }
    }
    bail!("refused: cannot determine the approver (neither USER nor LOGNAME is set)")
}

/// `ruling approve`: TTY stdin + the id typed back. The TTY check is a
/// barrier against the non-interactive agent Bash tool; it is NOT proof of
/// the approver's identity.
pub fn ruling_approve(tasks_path: &Path, id: &str, cancel: bool) -> Result<()> {
    let tasks = store::load(tasks_path)?;
    let t = find(&tasks, id)?;
    if t.status != STATUS_NEEDS_RULING {
        bail!(
            "refused: task {id} is {}, not needs-ruling; nothing to approve",
            t.status
        );
    }
    if !std::io::stdin().is_terminal() {
        bail!(
            "refused: `ruling approve` requires an interactive terminal on stdin (a human typing \
             the id back); stdin is not a TTY. This is a barrier against non-interactive agents, \
             not an identity check"
        );
    }
    let kind = t.ruling_kind.clone().unwrap_or_default();
    let why = t
        .rationale
        .clone()
        .or_else(|| t.untestable_reason.clone())
        .unwrap_or_default();
    let mut err = std::io::stderr();
    writeln!(err, "ruling for {id}: {}", t.title)?;
    writeln!(err, "  kind: {kind}")?;
    writeln!(err, "  rationale: {why}")?;
    if let Some(u) = &t.untestable_reason {
        writeln!(err, "  untestable_reason: {u}")?;
    }
    write!(
        err,
        "Type the task id to approve closing it as {}: ",
        if cancel { "cancelled" } else { "done" }
    )?;
    err.flush()?;
    let mut line = String::new();
    let n = std::io::stdin().lock().read_line(&mut line)?;
    if n == 0 {
        bail!("refused: no input read from the terminal");
    }
    if line.trim() != id {
        bail!(
            "refused: the typed id {:?} does not match {id}",
            line.trim()
        );
    }
    let by = approver()?;
    let now = now_unix()?;
    let status = if cancel { "cancelled" } else { STATUS_DONE };
    store::update_task(tasks_path, "ruling approve", id, |task, _| {
        if task.status != STATUS_NEEDS_RULING {
            bail!(
                "refused: task {id} is no longer needs-ruling (now {})",
                task.status
            );
        }
        let kind = task
            .ruling_kind
            .clone()
            .ok_or_else(|| anyhow!("refused: task {id} has no ruling_kind recorded"))?;
        let rationale = task
            .rationale
            .clone()
            .or_else(|| task.untestable_reason.clone())
            .ok_or_else(|| anyhow!("refused: task {id} has no rationale recorded"))?;
        task.closure = Some(Closure {
            reason: kind.clone(),
            duplicate_of: None,
            doc_only_commit: None,
            green: None,
            red: None,
            ruling: Some(RulingRecord {
                kind,
                rationale,
                approved_by: by.clone(),
                approved_at: now,
                approved_via: "tty".to_string(),
            }),
        });
        task.status = status.to_string();
        Ok(())
    })?;
    println!("{status}: {id} (ruling approved by {by} via tty)");
    Ok(())
}

/// `ruling withdraw`: needs-ruling → pending (evidence-gated done applies).
pub fn ruling_withdraw(tasks_path: &Path, id: &str) -> Result<()> {
    store::update_task(tasks_path, "ruling withdraw", id, |task, _| {
        if task.status != STATUS_NEEDS_RULING {
            bail!("refused: task {id} is {}, not needs-ruling", task.status);
        }
        task.status = STATUS_PENDING.to_string();
        task.ruling_kind = None;
        task.rationale = None;
        task.untestable_reason = None;
        Ok(())
    })?;
    println!("withdrawn: {id} (pending; closing it now needs evidence)");
    Ok(())
}

/// `ruling list`.
pub fn ruling_list(tasks_path: &Path) -> Result<()> {
    let tasks = store::load(tasks_path)?;
    let rows: Vec<&Task> = tasks
        .iter()
        .filter(|t| t.status == STATUS_NEEDS_RULING)
        .collect();
    println!("{} task(s) awaiting a human ruling", rows.len());
    for t in rows {
        let kind = t.ruling_kind.as_deref().unwrap_or("?");
        let why = t
            .untestable_reason
            .as_deref()
            .or(t.rationale.as_deref())
            .unwrap_or("");
        println!("{:<10} {:<11} {}  — {}", t.id, kind, t.title, why);
    }
    Ok(())
}

// ---- confirm -----------------------------------------------------------------

/// `confirm ID --repro-test CMD`: record the attempt; promote to pending only
/// on `reproduced`. Anything else leaves it unconfirmed and exits non-zero.
pub fn confirm(tasks_path: &Path, id: &str, repro_test: &str) -> Result<()> {
    let tasks = store::load(tasks_path)?;
    let t = find(&tasks, id)?;
    if t.status != STATUS_UNCONFIRMED {
        bail!("refused: task {id} is {}, not unconfirmed", t.status);
    }
    let now = now_unix()?;
    let cwd = std::env::current_dir()?;
    let repro = evidence::run_repro(&cwd, repro_test, now);
    let outcome = repro.outcome.clone();
    let detail = repro.detail.clone();
    store::update_task(tasks_path, "confirm", id, |task, _| {
        if task.status != STATUS_UNCONFIRMED {
            bail!(
                "refused: task {id} is no longer unconfirmed (now {})",
                task.status
            );
        }
        if repro.outcome == REPRO_REPRODUCED {
            task.status = STATUS_PENDING.to_string();
        }
        task.repro = Some(repro);
        Ok(())
    })?;
    if outcome == REPRO_REPRODUCED {
        println!("confirmed: {id} (reproduced; now pending)");
        Ok(())
    } else {
        bail!("not confirmed: {id} stays unconfirmed — repro outcome {outcome}: {detail}")
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
        if c.ruling.is_some() {
            return "ruling-approved";
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

/// The crate an untestable row belongs to, from its declared scope.
fn crate_of(t: &Task) -> String {
    t.touched_files
        .iter()
        .find_map(|f| {
            let mut parts = f.split('/');
            match (parts.next(), parts.next()) {
                (Some("crates"), Some(c)) if !c.is_empty() => Some(c.to_string()),
                _ => None,
            }
        })
        .unwrap_or_else(|| "(undeclared)".to_string())
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
    let mut untestable: std::collections::BTreeMap<String, Vec<serde_json::Value>> =
        std::collections::BTreeMap::new();
    for t in &tasks {
        let reason = t.untestable_reason.clone().or_else(|| {
            t.closure
                .as_ref()
                .and_then(|c| c.ruling.as_ref())
                .filter(|r| r.kind == RULING_UNTESTABLE)
                .map(|r| r.rationale.clone())
        });
        if let Some(r) = reason {
            untestable
                .entry(crate_of(t))
                .or_default()
                .push(serde_json::json!({"id": t.id, "status": t.status, "untestable_reason": r}));
        }
    }
    if as_json {
        let v = serde_json::json!({
            "rows": rows,
            "counts": counts,
            "untestable_by_crate": untestable,
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
    if !untestable.is_empty() {
        println!("untestable_reason by crate:");
        for (c, items) in &untestable {
            println!("  {c}: {}", items.len());
            for i in items {
                println!(
                    "    {} {}",
                    i["id"].as_str().unwrap_or(""),
                    i["untestable_reason"].as_str().unwrap_or("")
                );
            }
        }
    }
    Ok(())
}

/// The evidence label `list` shows for a row.
pub fn evidence_label(t: &Task) -> String {
    if t.status == STATUS_UNCONFIRMED {
        return match &t.repro {
            Some(r) => format!("suspicion({})", r.outcome),
            None => "suspicion".to_string(),
        };
    }
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
    } else if c.ruling.is_some() {
        "ruling"
    } else {
        "none"
    }
}
