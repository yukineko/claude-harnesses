//! Durable record of the last `sync` failure, so the failure is not dark.
//!
//! `sync`'s only caller is the `SessionEnd` hook declared in
//! `hooks/hooks.json`. A `SessionEnd` hook's exit code and stderr reach
//! neither the agent nor the user, so a non-zero exit is, on its own, an
//! invisible signal: "the store synced and had nothing to do" and "the store
//! has not synced in a month" look identical from outside (CLAUDE.md §1/§3).
//! Measured 2026-08-26: `sync` had been aborting in its pull phase since
//! 2026-07-23 and nothing anywhere said so.
//!
//! So a failure is written here, and the `UserPromptSubmit` hook — a channel
//! someone actually reads — surfaces it until a later `sync` succeeds and
//! clears it.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::store;

#[derive(Serialize, Deserialize)]
struct SyncFailure {
    /// Unix seconds of the failed attempt.
    at: u64,
    /// The error chain from the failed `sync`.
    detail: String,
}

fn marker_path() -> PathBuf {
    crate::config::home_dir()
        .join(".fugu-router")
        .join("sync-error.json")
}

/// Record that `sync` could not finish. Best-effort: if this cannot be
/// written, say so on stderr rather than pretending it was recorded — the
/// caller is already returning a non-zero exit, and losing the marker only
/// costs visibility, never correctness.
pub fn record(detail: &str) {
    let path = marker_path();
    let failure = SyncFailure {
        at: store::now_secs(),
        detail: detail.to_string(),
    };
    let body = match serde_json::to_string(&failure) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("fugu-router: could not serialize the sync-failure marker: {e}");
            return;
        }
    };
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            eprintln!(
                "fugu-router: could not create {} for the sync-failure marker: {e}",
                parent.display()
            );
            return;
        }
    }
    if let Err(e) = std::fs::write(&path, body) {
        eprintln!(
            "fugu-router: could not write the sync-failure marker to {}: {e}",
            path.display()
        );
    }
}

/// Drop the marker after a successful `sync`. A warning that never clears is
/// one nobody reads, which is the same as being invisible.
pub fn clear() {
    let path = marker_path();
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => eprintln!(
            "fugu-router: could not clear the sync-failure marker at {}: {e}",
            path.display()
        ),
    }
}

/// The notice to surface, if the last `sync` failed.
///
/// An unreadable or unparseable marker still returns a notice. Its presence
/// already says "the last sync failed"; being unable to read the detail is a
/// second problem, not a reason to report the store as healthy.
pub fn pending() -> Option<String> {
    let path = marker_path();
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            return Some(format!(
                "fugu-router: could not sync the record store — a failure marker exists at {} \
                 but is unreadable ({e}). Episodes recorded on this machine may not be \
                 reaching `sync_repo`. Run `fugu-router sync` to see the real error.",
                path.display()
            ))
        }
    };
    match serde_json::from_str::<SyncFailure>(&raw) {
        Ok(f) => Some(format!(
            "fugu-router: could not sync the record store (last attempt at unix {}): {}. \
             Episodes recorded on this machine are not reaching `sync_repo`. \
             Run `fugu-router sync` to see the full error.",
            f.at, f.detail
        )),
        Err(e) => Some(format!(
            "fugu-router: could not sync the record store — the failure marker at {} \
             did not parse ({e}). Episodes recorded on this machine may not be \
             reaching `sync_repo`. Run `fugu-router sync` to see the real error.",
            path.display()
        )),
    }
}
