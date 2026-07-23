//! PDO session-anchor integration (DESIGN §4.4 / §4.6b).
//!
//! stuckguard already tracks the edited file of every tool call (see `sig`), so
//! it can cheaply answer two anchor questions without new detection machinery:
//!
//! - **scope drift (§4.4):** are recent edits landing *outside* the files this
//!   session said it would touch? (advisory nudge, opt-in)
//! - **heartbeat piggyback (§4.6b):** keep this session's claim/lease alive on
//!   every tool call so a long single task isn't falsely reaped and stolen.
//!
//! The session's anchor (scope / run / key) lives in `overwatch`'s lease
//! registry; we read it fail-soft via `overwatch lease --session <id> --json`.
//! Everything here degrades to a silent no-op when overwatch is absent, the
//! session holds no lease, or the JSON can't be parsed — never breaking a turn.

use std::process::Command;

use harness_core::boundary;
use serde::Deserialize;

use crate::sig::Event;

/// The current session's live anchor, as read from the overwatch lease.
#[derive(Debug, Clone, Deserialize)]
pub struct SessionAnchor {
    /// The lease key (used for `overwatch heartbeat --key`).
    #[serde(default)]
    pub key: String,
    /// The run id (used for `condukt state heartbeat --run`).
    #[serde(default)]
    pub run_id: String,
    /// Files/globs this session is responsible for. Empty = no fixed scope.
    #[serde(default)]
    pub scope: Vec<String>,
}

/// Parse an `overwatch lease --json` line into a `SessionAnchor`. Extra fields
/// (title, timestamps, done_criteria) are ignored. Pure — unit tested.
pub fn parse_anchor(json: &str) -> Option<SessionAnchor> {
    serde_json::from_str::<SessionAnchor>(json.trim()).ok()
}

/// Read the live anchor for `session_id` by shelling out to overwatch. Fail-soft:
/// returns `None` if overwatch is missing, exits non-zero (no live lease), or
/// emits unparseable output.
///
/// Routed through `harness_core::boundary::run` so "overwatch could not be run
/// at all" (`Undetermined`) is distinguished from "overwatch ran and said no
/// lease" (`Known`, non-zero exit) at the call site — but stuckguard is a pure
/// advisory hook (never blocks), so both degrade the same way this function
/// always has: a silent `None`, same as before this migration.
pub fn fetch_session_anchor(session_id: &str) -> Option<SessionAnchor> {
    let mut cmd = Command::new("overwatch");
    cmd.args(["lease", "--session", session_id, "--json"]);
    let stdout = boundary::run(&mut cmd)
        .require()
        .ok()?
        .stdout_on_success()
        .require()
        .ok()?;
    parse_anchor(&stdout)
}

/// Keep this session's claim/lease alive (§4.6b). Fires a `condukt` and an
/// `overwatch` heartbeat; both are best-effort — errors and missing binaries are
/// ignored (the nudge path must never be blocked by this side effect).
pub fn heartbeat_piggyback(anchor: &SessionAnchor) {
    if !anchor.run_id.is_empty() {
        let mut cmd = Command::new("condukt");
        cmd.args(["state", "heartbeat", "--run", &anchor.run_id]);
        let _ = boundary::run(&mut cmd);
    }
    if !anchor.key.is_empty() {
        let mut cmd = Command::new("overwatch");
        cmd.args(["heartbeat", "--key", &anchor.key]);
        let _ = boundary::run(&mut cmd);
    }
}

/// Literal path prefix of a glob: the part before the first glob metacharacter,
/// trailing `/` trimmed.
fn glob_prefix(g: &str) -> &str {
    let end = g.find(['*', '?', '[']).unwrap_or(g.len());
    g[..end].trim_end_matches('/')
}

/// Is `file` within the anchor `scope`? Matches by substring on each glob's
/// literal prefix, so a tool's absolute `file_path`
/// (`/home/u/repo/crates/x/src/a.rs`) still matches a repo-relative scope glob
/// (`crates/x/src/**`). A bare glob (`**`, empty prefix) covers everything.
/// Deliberately lenient: over-matching "in scope" only makes the drift advisory
/// *less* likely to fire (conservative for an opt-in nudge).
fn file_in_scope(file: &str, scope: &[String]) -> bool {
    scope.iter().any(|g| {
        let p = glob_prefix(g);
        p.is_empty() || file.contains(p)
    })
}

/// Detect PDO scope drift (§4.4): the trailing run of consecutive *edited* files
/// (events carrying a `file`) that all fall outside `scope`. Returns the drifted
/// files (deduped, in order) when that run reaches `threshold`, else `None`.
/// Non-edit events (no `file`) are skipped; an in-scope edit resets the run.
/// `None` when scope is empty (no anchor to compare against) or threshold is 0.
pub fn scope_drift(events: &[Event], scope: &[String], threshold: usize) -> Option<Vec<String>> {
    if scope.is_empty() || threshold == 0 {
        return None;
    }
    let mut run: Vec<String> = Vec::new();
    for ev in events {
        let Some(file) = &ev.file else { continue };
        if file_in_scope(file, scope) {
            run.clear();
        } else {
            run.push(file.clone());
        }
    }
    if run.len() < threshold {
        return None;
    }
    // Dedup while preserving order.
    let mut seen = std::collections::BTreeSet::new();
    let drifted: Vec<String> = run.into_iter().filter(|f| seen.insert(f.clone())).collect();
    Some(drifted)
}

/// The advisory nudge text for scope drift.
pub fn scope_drift_message(scope: &[String], drifted: &[String]) -> String {
    format!(
        "🧭 stuckguard: このセッションは {} の担当のはずですが、直近の編集は {} でした。\
         scope を広げる意図なら anchor を更新してください（`overwatch begin --key ... --scope ...` を再実行）。\
         そうでなければ元のタスクに戻ってください。",
        scope.join(", "),
        drifted.join(", "),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn edit(seq: u64, file: &str) -> Event {
        Event {
            seq,
            tool: "Edit".to_string(),
            sig: format!("sig-{seq}"),
            tokens: BTreeSet::new(),
            file: Some(file.to_string()),
            old_h: None,
            new_h: None,
            error: false,
            failed_test_digest: None,
        }
    }

    fn non_edit(seq: u64) -> Event {
        Event {
            seq,
            tool: "Bash".to_string(),
            sig: format!("sig-{seq}"),
            tokens: BTreeSet::new(),
            file: None,
            old_h: None,
            new_h: None,
            error: false,
            failed_test_digest: None,
        }
    }

    #[test]
    fn parse_anchor_reads_scope_and_ignores_extra_fields() {
        let json = r#"{"key":"k","title":"t","run_id":"r","scope":["crates/x/src/**"],"done_criteria":"green","claimed_at":1}"#;
        let a = parse_anchor(json).expect("parses");
        assert_eq!(a.key, "k");
        assert_eq!(a.run_id, "r");
        assert_eq!(a.scope, vec!["crates/x/src/**".to_string()]);
    }

    #[test]
    fn scope_drift_fires_after_threshold_out_of_scope_edits() {
        let scope = vec!["crates/overwatch/src/**".to_string()];
        let events = vec![
            edit(1, "/home/u/repo/crates/foo/src/a.rs"),
            edit(2, "crates/bar/src/b.rs"),
            edit(3, "crates/baz/src/c.rs"),
        ];
        let drifted = scope_drift(&events, &scope, 3).expect("drift detected");
        assert_eq!(drifted.len(), 3);
    }

    #[test]
    fn scope_drift_absolute_path_matches_relative_scope_glob() {
        // An absolute file_path inside the scope's dir is IN scope -> no drift.
        let scope = vec!["crates/overwatch/src/**".to_string()];
        let events = vec![
            edit(1, "/home/u/repo/crates/overwatch/src/store.rs"),
            edit(2, "/home/u/repo/crates/overwatch/src/lease.rs"),
            edit(3, "/home/u/repo/crates/overwatch/src/main.rs"),
        ];
        assert!(scope_drift(&events, &scope, 3).is_none());
    }

    #[test]
    fn in_scope_edit_resets_the_run() {
        let scope = vec!["crates/overwatch/src/**".to_string()];
        let events = vec![
            edit(1, "crates/foo/a.rs"),        // out
            edit(2, "crates/foo/b.rs"),        // out
            edit(3, "crates/overwatch/src/x"), // IN -> resets
            edit(4, "crates/foo/c.rs"),        // out (run = 1)
        ];
        assert!(scope_drift(&events, &scope, 3).is_none());
    }

    #[test]
    fn non_edit_events_do_not_break_the_run() {
        let scope = vec!["crates/overwatch/src/**".to_string()];
        let events = vec![
            edit(1, "crates/foo/a.rs"),
            non_edit(2), // skipped
            edit(3, "crates/foo/b.rs"),
            non_edit(4), // skipped
            edit(5, "crates/foo/c.rs"),
        ];
        assert!(scope_drift(&events, &scope, 3).is_some());
    }

    #[test]
    fn empty_scope_never_drifts() {
        let events = vec![edit(1, "a.rs"), edit(2, "b.rs"), edit(3, "c.rs")];
        assert!(scope_drift(&events, &[], 3).is_none());
    }
}
