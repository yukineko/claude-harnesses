//! compass-side helpers over [`harness_core::discovery`] — the machine-scope
//! shared discovery store that lets concurrent compass/scout sessions avoid
//! re-surfacing a task another session already discovered.
//!
//! This module is the thin compass layer: session-id resolution, the
//! `record`/`select`/`list`/`migrate-legacy` command bodies, the opportunity-add
//! discovery hook, and the cross-session dedup filter. The persistence (append-only
//! JSONL, keyed on the main worktree root) lives entirely in
//! `harness_core::discovery`; we never reinvent it.
//!
//! Failure is reported, not hidden:
//! - the `discovery` subcommands (`record`/`select`/`list`/`migrate-legacy`) return
//!   an error — exit non-zero with `undetermined` on stderr — when the store cannot
//!   be resolved, read or written. `list` never prints `[]` for such a store;
//! - the callers that sit inside someone else's turn (`gap`, `route`, the
//!   opportunity-add hook) do NOT fail because of the dedup store, but they never
//!   claim dedup ran either: `gap` reports `discovery_dedup`, `route` prints
//!   `dedup unknown`, the hook warns on stderr. When dedup is unknown every
//!   candidate is KEPT.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Result};
use harness_core::discovery::{self, DiscoveryRecord, Status};
use harness_core::verdict::Determination;
use serde::Serialize;

use crate::opportunity::Opportunity;

/// The env var carrying the active Claude Code session id. Empty/unset resolves
/// to a stable default so single-session use still records coherent rows.
const SESSION_ENV: &str = "CLAUDE_CODE_SESSION_ID";

/// Stable default session id when none is supplied (and the env is empty/unset).
const DEFAULT_SESSION: &str = "local";

/// Resolve the session id from the env (`CLAUDE_CODE_SESSION_ID`), falling back
/// to a stable default ([`DEFAULT_SESSION`]) when unset/blank. A missing or
/// non-UTF8 env var resolves to the default rather than erroring.
pub fn session_from_env() -> String {
    std::env::var(SESSION_ENV)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_SESSION.to_string())
}

/// Resolve a session id: an explicit non-blank `arg` wins, else fall back to the
/// env-or-default ([`session_from_env`]).
fn resolve_session(arg: Option<String>) -> String {
    match arg {
        Some(s) if !s.trim().is_empty() => s.trim().to_string(),
        _ => session_from_env(),
    }
}

/// Current wall-clock as unix seconds; 0 on a pre-epoch clock.
fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Turn an undetermined store answer into an error whose message says so.
fn undetermined_err(what: &str, why: &harness_core::verdict::Undet) -> anyhow::Error {
    anyhow::anyhow!("discovery store undetermined ({what}): {why}")
}

/// Append a `Discovered` row for `title` under `session`. Errors (message
/// contains `undetermined`) when the row could not be stored. Shared by the
/// `record` command AND the opportunity-add hook (which downgrades the error to a
/// warning).
pub fn append_discovered(cwd: &Path, title: &str, session: &str) -> Result<()> {
    let rec = DiscoveryRecord {
        fingerprint: discovery::fingerprint(title),
        session_id: session.to_string(),
        status: Status::Discovered,
        created_at: now_unix(),
        title: title.to_string(),
    };
    match discovery::append(cwd, &rec) {
        Determination::Known(()) => Ok(()),
        Determination::Undetermined(why) => Err(undetermined_err("record not stored", &why)),
    }
}

/// `compass discovery record`: append a `Discovered` row for `title`. Session
/// from `--session-id` else env-or-default. Errors when the row was not stored.
pub fn record_command(cwd: &Path, title: String, session_id: Option<String>) -> Result<()> {
    let session = resolve_session(session_id);
    append_discovered(cwd, &title, &session)
}

/// `compass discovery select`: resolve a fingerprint (from `--fingerprint` else
/// `fingerprint(--title)`) and mark every matching row `Selected`. With neither,
/// print a short usage note to stderr and return Ok (a usage no-op, not a store
/// failure). Errors when the store could not be updated.
pub fn select_command(
    cwd: &Path,
    fingerprint: Option<String>,
    title: Option<String>,
) -> Result<()> {
    let Some(fp) = resolve_fingerprint(fingerprint, title) else {
        eprintln!("compass: discovery select needs --fingerprint or --title (no-op)");
        return Ok(());
    };
    match discovery::mark_selected(cwd, &fp) {
        Determination::Known(()) => Ok(()),
        Determination::Undetermined(why) => Err(undetermined_err("nothing marked", &why)),
    }
}

/// Resolve a fingerprint from `--fingerprint` (used verbatim if non-blank) else
/// from `fingerprint(--title)`. `None` iff neither yields a usable value.
fn resolve_fingerprint(fingerprint: Option<String>, title: Option<String>) -> Option<String> {
    if let Some(fp) = fingerprint {
        let fp = fp.trim();
        if !fp.is_empty() {
            return Some(fp.to_string());
        }
    }
    if let Some(title) = title {
        let title = title.trim();
        if !title.is_empty() {
            return Some(discovery::fingerprint(title));
        }
    }
    None
}

/// `compass discovery list`: load all rows. A never-written store is `[]` /
/// "(no discovery records)"; an undetermined store (unresolvable root, unreadable
/// file, corrupt line) is an error and prints nothing on stdout.
pub fn list_command(cwd: &Path, json: bool) -> Result<()> {
    let records = match discovery::load(cwd) {
        Determination::Known(r) => r,
        Determination::Undetermined(why) => return Err(undetermined_err("cannot list", &why)),
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&records)?);
    } else if records.is_empty() {
        println!("(no discovery records)");
    } else {
        for r in &records {
            let status = match r.status {
                Status::Discovered => "discovered",
                Status::Selected => "selected",
            };
            println!(
                "- [{}] {} \"{}\" (session {})",
                r.fingerprint, status, r.title, r.session_id
            );
        }
    }
    Ok(())
}

/// `compass discovery migrate-legacy`: copy legacy worktree-keyed rows into the
/// canonical store (see [`discovery::migrate_legacy`]). Prints one JSON summary
/// object; errors (nothing written) when anything is undetermined.
pub fn migrate_legacy_command(cwd: &Path) -> Result<()> {
    match discovery::migrate_legacy(cwd) {
        Determination::Known(summary) => {
            println!("{}", serde_json::to_string(&summary)?);
            Ok(())
        }
        Determination::Undetermined(why) => {
            bail!("discovery store undetermined (migration not applied): {why}")
        }
    }
}

/// Whether cross-session dedup actually ran.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum DedupStatus {
    /// The store was read; duplicates (if any) were dropped.
    Applied,
    /// The store could not be read; NOTHING was dropped.
    Undetermined { reason: String },
}

/// Drop opportunities whose title was already discovered by ANOTHER session
/// (cross-session dedup, DoD#2). The single canonical place this dedup lives,
/// called at both surface points (`gap` and the `route` handoff).
///
/// A never-written store drops nothing and is `Applied` (there is genuinely
/// nothing to dedup against). If the store is undetermined (unresolvable root,
/// unreadable file, corrupt line) ALL candidates are kept — surfacing a possible
/// duplicate is the safe side — and the status says `Undetermined` with the
/// reason, so the caller can tell the user that dedup did not run.
pub fn filter_undiscovered_by_others(
    cwd: &Path,
    my_session: &str,
    opportunities: Vec<Opportunity>,
) -> (Vec<Opportunity>, DedupStatus) {
    // Probe the store first so "applied" is never claimed vacuously (e.g. for an
    // empty candidate list over an unreadable store).
    if let Determination::Undetermined(why) = discovery::load(cwd) {
        return (
            opportunities,
            DedupStatus::Undetermined {
                reason: why.to_string(),
            },
        );
    }
    let mut keep = Vec::with_capacity(opportunities.len());
    for o in &opportunities {
        let fp = discovery::fingerprint(&o.title);
        match discovery::already_discovered_by_other(cwd, &fp, my_session) {
            Determination::Known(dup) => keep.push(!dup),
            Determination::Undetermined(why) => {
                return (
                    opportunities,
                    DedupStatus::Undetermined {
                        reason: why.to_string(),
                    },
                )
            }
        }
    }
    let kept = opportunities
        .into_iter()
        .zip(keep)
        .filter_map(|(o, k)| k.then_some(o))
        .collect();
    (kept, DedupStatus::Applied)
}
