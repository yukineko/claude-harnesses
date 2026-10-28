//! Automatic session presence: the hook-written registry behind the
//! `active_sessions` part of `overwatch status`.
//!
//! The lease ledger (`leases.json`) only ever learns about a session when that
//! session runs `overwatch begin`. A session that never does so left no trace,
//! so `status` printed `(none)` while sessions were live. This module closes
//! that gap: `overwatch presence` is wired to the Claude Code SessionStart,
//! PostToolUse (Edit|Write|MultiEdit|NotebookEdit) and SessionEnd hooks, reads
//! the hook payload JSON on stdin, and records it in `presence.json` beside
//! `leases.json` (same store, keyed by the main worktree root, so every
//! checkout of the repository shares it).
//!
//! Fail-closed contract (CLAUDE.md section 3): an existing `presence.json`
//! that cannot be read or parsed aborts the hook with a non-zero exit and is
//! NEVER replaced by an empty registry, because overwriting it would destroy
//! the very record whose absence `status` would then read as "nobody is here".
//! A malformed payload (no `session_id`, no `cwd`, not JSON) is likewise an
//! error, not a silent success.

use crate::lock::LeaseLock;
use crate::store;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;

/// Entries whose `last_seen` is older than this AND that are not `live`
/// (see `prunable`) are dropped on every write.
pub const PRUNE_AFTER_SECS: i64 = 24 * 3600;

/// Tools whose `tool_input` path is recorded into `written_paths`.
const WRITE_TOOLS: [&str; 4] = ["Edit", "Write", "MultiEdit", "NotebookEdit"];

/// One session as recorded by the hooks.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresenceEntry {
    pub session_id: String,
    /// cwd of the most recent hook payload.
    pub cwd: String,
    /// Git toplevel of `cwd` (`projkey::repo_root`), i.e. the checkout or
    /// linked worktree the session works in.
    pub worktree: String,
    /// Claude Code transcript file; its mtime is the liveness signal.
    #[serde(default)]
    pub transcript_path: String,
    /// Deduplicated paths from PostToolUse Edit/Write/MultiEdit/NotebookEdit.
    #[serde(default)]
    pub written_paths: Vec<String>,
    /// Unix seconds of the most recent hook for this session.
    pub last_seen: i64,
    /// Set by SessionEnd.
    #[serde(default)]
    pub ended: bool,
}

/// session_id -> entry.
pub type PresenceRegistry = BTreeMap<String, PresenceEntry>;

/// Load `presence.json`. Absent => empty registry (nothing was ever
/// recorded); unreadable or corrupt => `Err` (never an empty registry).
pub fn load_presence(cwd: &Path) -> Result<PresenceRegistry> {
    let path = store::presence_path(cwd)?;
    match harness_core::boundary::read_to_string(&path) {
        harness_core::verdict::Determination::Known(None) => Ok(PresenceRegistry::new()),
        harness_core::verdict::Determination::Known(Some(txt)) => {
            serde_json::from_str::<PresenceRegistry>(&txt).map_err(|e| {
                anyhow::anyhow!(
                    "presence.json present but corrupt at {}: {e}",
                    path.display()
                )
            })
        }
        harness_core::verdict::Determination::Undetermined(reason) => {
            bail!(
                "presence.json could not be read at {}: {reason}",
                path.display()
            )
        }
    }
}

/// Atomic write (temp + rename) of the registry.
fn save_presence(cwd: &Path, reg: &PresenceRegistry) -> Result<()> {
    let path = store::presence_path(cwd)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(reg)?;
    let tmp = store::unique_tmp(&path, "json");
    std::fs::write(&tmp, &json)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

fn str_field<'a>(payload: &'a Value, key: &str) -> Option<&'a str> {
    payload.get(key).and_then(Value::as_str)
}

/// Whether `e` may be dropped from the registry on a write.
///
/// An entry is prunable only when BOTH hold: `last_seen` is older than
/// `PRUNE_AFTER_SECS`, AND `liveness` (the same function `aggregate` uses to
/// label `active_sessions`) says it is anything other than `live`. `last_seen`
/// only advances on that session's own hooks, so a long-running session that
/// fired no hook for >24h but whose transcript is still being written is
/// live and must not be deleted by another session's write.
///
/// `unknown` (transcript path empty/missing/unreadable, or an untrustworthy
/// mtime) is pruned once past the age cutoff. It is never judged live, so it
/// cannot shield an entry; keeping such entries forever would leak them
/// unboundedly (a deleted transcript never becomes live again), while the
/// 24h grace window plus the absence of any hook in that time is the only
/// evidence we have. Within the window the entry stays visible as `unknown`.
fn prunable(e: &PresenceEntry, now: i64) -> bool {
    now - e.last_seen > PRUNE_AFTER_SECS && liveness(e, now) != LIVE
}

/// Registry update for one hook payload. Pure except that pruning stats the
/// transcript of entries past the age cutoff (via `liveness`).
///
/// SessionStart (re)opens the session, SessionEnd marks it ended, PostToolUse
/// of a write tool appends the touched path; any other event only refreshes
/// `last_seen`/`cwd`/`transcript_path`.
pub fn apply_event(reg: &mut PresenceRegistry, payload: &Value, now: i64) -> Result<()> {
    let sid = match str_field(payload, "session_id") {
        Some(s) if !s.is_empty() => s,
        _ => bail!("hook payload has no session_id"),
    };
    let cwd = match str_field(payload, "cwd") {
        Some(s) if !s.is_empty() => s,
        _ => bail!("hook payload has no cwd"),
    };
    let event = str_field(payload, "hook_event_name").unwrap_or("");

    let entry = reg.entry(sid.to_string()).or_default();
    entry.session_id = sid.to_string();
    entry.cwd = cwd.to_string();
    entry.worktree = harness_core::projkey::repo_root(Path::new(cwd))
        .to_string_lossy()
        .into_owned();
    if let Some(t) = str_field(payload, "transcript_path") {
        if !t.is_empty() {
            entry.transcript_path = t.to_string();
        }
    }
    entry.last_seen = now;

    match event {
        "SessionStart" => entry.ended = false,
        "SessionEnd" => entry.ended = true,
        "PostToolUse" => {
            entry.ended = false;
            let tool = str_field(payload, "tool_name").unwrap_or("");
            if WRITE_TOOLS.contains(&tool) {
                let input = payload.get("tool_input");
                let path = input
                    .and_then(|i| i.get("file_path").or_else(|| i.get("notebook_path")))
                    .and_then(Value::as_str);
                if let Some(p) = path {
                    if !p.is_empty() && !entry.written_paths.iter().any(|w| w == p) {
                        entry.written_paths.push(p.to_string());
                    }
                }
            }
        }
        _ => {}
    }

    reg.retain(|_, e| !prunable(e, now));
    Ok(())
}

/// Record one payload under the store lock.
pub fn record(payload: &Value, now: i64) -> Result<()> {
    let cwd = match str_field(payload, "cwd") {
        Some(s) if !s.is_empty() => std::path::PathBuf::from(s),
        _ => bail!("hook payload has no cwd"),
    };
    let _lock = LeaseLock::acquire_strict(&cwd)
        .map_err(|e| anyhow::anyhow!("presence lock not acquired: {e}"))?;
    let mut reg = load_presence(&cwd)?;
    apply_event(&mut reg, payload, now)?;
    save_presence(&cwd, &reg)
}

/// `overwatch presence`: read the hook payload from stdin and record it.
pub fn run_hook() -> Result<()> {
    let mut buf = String::new();
    std::io::stdin()
        .read_to_string(&mut buf)
        .context("cannot read hook payload from stdin")?;
    let payload: Value =
        serde_json::from_str(&buf).context("hook payload on stdin is not valid JSON")?;
    record(&payload, store::now())
}

/// Liveness labels reported in `active_sessions`.
pub const LIVE: &str = "live";
pub const STALE: &str = "stale";
pub const UNKNOWN: &str = "unknown";
pub const ENDED: &str = "ended";

/// Judge one entry from the transcript mtime vs the lease TTL.
/// Missing/unreadable transcript, or an mtime too far in the future to trust,
/// is `unknown` -- never `live`.
pub fn liveness(entry: &PresenceEntry, now: i64) -> &'static str {
    if entry.ended {
        return ENDED;
    }
    if entry.transcript_path.is_empty() {
        return UNKNOWN;
    }
    let mtime = std::fs::metadata(&entry.transcript_path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64);
    match mtime {
        None => UNKNOWN,
        Some(m) => {
            let age = now - m;
            if age < -60 {
                UNKNOWN
            } else if age <= store::LEASE_TTL_SECS {
                LIVE
            } else {
                STALE
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use serde_json::json;

    fn ev(name: &str, sid: &str) -> Value {
        json!({"session_id":sid,"cwd":"/nonexistent/x","transcript_path":"/nonexistent/t",
               "hook_event_name":name})
    }

    #[test]
    fn missing_session_id_or_cwd_is_an_error() {
        let mut reg = PresenceRegistry::new();
        assert!(apply_event(&mut reg, &json!({"cwd":"/x"}), 1).is_err());
        assert!(apply_event(&mut reg, &json!({"session_id":"a"}), 1).is_err());
        assert!(reg.is_empty());
    }

    #[test]
    fn written_paths_are_deduplicated_and_notebook_path_is_read() {
        let mut reg = PresenceRegistry::new();
        apply_event(&mut reg, &ev("SessionStart", "a"), 10).unwrap();
        for tool_input in [
            json!({"file_path":"/r/a.rs"}),
            json!({"file_path":"/r/a.rs"}),
            json!({"notebook_path":"/r/n.ipynb"}),
        ] {
            let mut p = ev("PostToolUse", "a");
            p["tool_name"] = json!("Edit");
            p["tool_input"] = tool_input;
            apply_event(&mut reg, &p, 11).unwrap();
        }
        assert_eq!(reg["a"].written_paths, vec!["/r/a.rs", "/r/n.ipynb"]);
    }

    #[test]
    fn non_write_tool_records_no_path() {
        let mut reg = PresenceRegistry::new();
        let mut p = ev("PostToolUse", "a");
        p["tool_name"] = json!("Read");
        p["tool_input"] = json!({"file_path":"/r/a.rs"});
        apply_event(&mut reg, &p, 1).unwrap();
        assert!(reg["a"].written_paths.is_empty());
    }

    #[test]
    fn session_end_marks_ended_and_start_reopens() {
        let mut reg = PresenceRegistry::new();
        apply_event(&mut reg, &ev("SessionEnd", "a"), 5).unwrap();
        assert!(reg["a"].ended);
        apply_event(&mut reg, &ev("SessionStart", "a"), 6).unwrap();
        assert!(!reg["a"].ended);
    }

    #[test]
    fn entries_older_than_24h_are_pruned() {
        let mut reg = PresenceRegistry::new();
        apply_event(&mut reg, &ev("SessionStart", "old"), 0).unwrap();
        apply_event(&mut reg, &ev("SessionStart", "new"), PRUNE_AFTER_SECS + 1).unwrap();
        assert!(!reg.contains_key("old"));
        assert!(reg.contains_key("new"));
    }

    #[test]
    fn liveness_is_never_live_for_ended_or_missing_transcript() {
        let mut e = PresenceEntry {
            transcript_path: "/nonexistent/t".into(),
            ..Default::default()
        };
        assert_eq!(liveness(&e, 100), UNKNOWN);
        e.ended = true;
        assert_eq!(liveness(&e, 100), ENDED);
        e.ended = false;
        e.transcript_path = String::new();
        assert_eq!(liveness(&e, 100), UNKNOWN);
    }
}
