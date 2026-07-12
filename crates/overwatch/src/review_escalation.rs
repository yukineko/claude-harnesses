//! Foreign-file bridge: read condukt's durable escalation queue
//! (`escalate.rs`) into the overwatch review-queue as a new
//! [`crate::review_queue::EntryKind::Escalation`] source.
//!
//! # Why a foreign-file read, not a crate dependency
//!
//! The workspace's dependency direction is `harness-core <- overwatch <-
//! blastguard <- condukt`. condukt already depends on overwatch, so
//! `overwatch` taking a `condukt` crate dependency would be a cycle. Instead
//! this module reads condukt's `escalations.json` **by path**, as fail-soft
//! foreign JSON, using ONLY `harness_core` primitives — the SAME
//! `harness_core::config::base_dir` / `harness_core::projkey::{repo_root,
//! project_key}` symbols [`crate::store`] already uses to derive overwatch's
//! own storage root.
//!
//! # Known limitation
//!
//! This reads condukt's **default** state path
//! (`~/.condukt/state/<project-key>/escalations.json`). If a user overrides
//! `state_dir` in `~/.condukt/config.toml`, those escalations live elsewhere
//! and will simply not be found here — no error, they just don't appear
//! (fail-soft graceful degrade, consistent with every other source in
//! `review_queue.rs`). Parsing condukt's own config across the crate boundary
//! is deliberately out of scope, to keep this coupling to the minimal file
//! contract described below.
//!
//! # File contract
//!
//! condukt's on-disk shape (`crates/condukt/src/escalate.rs`) is
//! `Registry { escalations: Vec<Escalation> }`. [`ConduktEscalation`] is a
//! MINIMAL mirror carrying only the fields this bridge needs (`id`, `run`,
//! `task`, `question`, `resolved`, `created_at`); condukt's other fields
//! (`options`, `recommended`, `chosen`) are silently ignored by serde. Every
//! mirrored field is `#[serde(default)]` so a partial/older condukt record
//! (or a future condukt version that drops a field) still parses instead of
//! failing the whole read.

use serde::Deserialize;
use std::path::{Path, PathBuf};

/// Minimal mirror of condukt's `escalate::Escalation` — only the fields
/// overwatch's review-queue needs. See the module doc for the cross-tool file
/// contract this mirrors.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ConduktEscalation {
    /// Stable content-hash id assigned by condukt.
    #[serde(default)]
    pub id: String,
    /// The condukt run this question belongs to.
    #[serde(default)]
    pub run: String,
    /// The task within the run that is blocked on this answer.
    #[serde(default)]
    pub task: String,
    /// The question being asked.
    #[serde(default)]
    pub question: String,
    /// Whether a human has already answered.
    #[serde(default)]
    pub resolved: bool,
    /// When the escalation was enqueued (unix seconds).
    #[serde(default)]
    pub created_at: i64,
}

/// Mirror of condukt's on-disk `Registry` wrapper: an object with an
/// `escalations` array.
#[derive(Debug, Default, Deserialize)]
struct Registry {
    #[serde(default)]
    escalations: Vec<ConduktEscalation>,
}

/// Parse condukt's `escalations.json` text and return only the still-OPEN
/// (`!resolved`) records. PURE and total: a parse error (corrupt/garbage
/// text) or empty input returns an empty vec rather than erroring or
/// panicking — mirrors the fail-soft contract every other `review_queue.rs`
/// source follows.
pub fn parse_open_escalations(txt: &str) -> Vec<ConduktEscalation> {
    let reg: Registry = serde_json::from_str(txt).unwrap_or_default();
    reg.escalations
        .into_iter()
        .filter(|e| !e.resolved)
        .collect()
}

/// Derive the DEFAULT path to condukt's `escalations.json` for the project
/// rooted at (or above) `cwd`:
/// `harness_core::config::base_dir("condukt")/state/<project-key>/escalations.json`
/// — mirroring condukt's own `escalate.rs::escalations_path` /
/// `config.rs::base_dir` derivation, and reusing the exact `harness_core`
/// symbols [`crate::store`] already calls for overwatch's own storage root.
/// See the module doc for the known non-default-`state_dir` limitation.
pub fn condukt_escalations_path(cwd: &Path) -> PathBuf {
    let base = harness_core::config::base_dir("condukt");
    let repo_root = harness_core::projkey::repo_root(cwd);
    let project_key = harness_core::projkey::project_key(&repo_root);
    base.join("state")
        .join(project_key)
        .join("escalations.json")
}

/// Read condukt's open escalations for the project at `cwd`, fail-soft: a
/// missing file (no condukt ever ran here, or nothing is open), an unreadable
/// file, or corrupt JSON all yield an empty vec rather than an error — so a
/// project with no condukt escalations contributes zero rows to the review
/// queue, never breaking the command.
pub fn read_open_escalations(cwd: &Path) -> Vec<ConduktEscalation> {
    let path = condukt_escalations_path(cwd);
    match std::fs::read_to_string(&path) {
        Ok(txt) => parse_open_escalations(&txt),
        Err(_) => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_open_escalations_keeps_only_unresolved() {
        let txt = r#"{
            "escalations": [
                {"id": "esc-1", "run": "r1", "task": "t1", "question": "Q1", "options": ["a","b"], "recommended": 0, "created_at": 100, "resolved": false},
                {"id": "esc-2", "run": "r1", "task": "t2", "question": "Q2", "options": ["a"], "recommended": 0, "created_at": 200, "resolved": true, "chosen": "a"}
            ]
        }"#;
        let open = parse_open_escalations(txt);
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].id, "esc-1");
        assert!(!open[0].resolved);
    }

    #[test]
    fn parse_open_escalations_corrupt_text_is_empty() {
        assert!(parse_open_escalations("not json at all {{{").is_empty());
    }

    #[test]
    fn parse_open_escalations_empty_text_is_empty() {
        assert!(parse_open_escalations("").is_empty());
    }

    #[test]
    fn parse_open_escalations_missing_fields_are_missing_tolerant() {
        // A partial/older record (missing `id`, extra unknown fields ignored)
        // must still parse rather than failing the whole read.
        let txt = r#"{"escalations": [{"run": "r1", "task": "t1", "question": "Q", "created_at": 1, "resolved": false, "extra_condukt_only_field": 42}]}"#;
        let open = parse_open_escalations(txt);
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].id, "");
        assert_eq!(open[0].run, "r1");
    }

    #[test]
    fn read_open_escalations_missing_path_is_empty_no_panic() {
        let tmp = std::env::temp_dir().join(format!(
            "overwatch-review-escalation-missing-{}",
            std::process::id()
        ));
        // Intentionally never created — the condukt path under it is absent.
        assert!(read_open_escalations(&tmp).is_empty());
    }

    #[test]
    fn condukt_escalations_path_is_rooted_under_condukt_state() {
        let cwd = std::env::temp_dir();
        let path = condukt_escalations_path(&cwd);
        assert!(path.ends_with("escalations.json"));
        let s = path.to_string_lossy();
        assert!(s.contains(".condukt"));
        assert!(s.contains("state"));
    }
}
