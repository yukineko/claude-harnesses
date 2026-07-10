/// Event store and lease storage backend.
use crate::event::LifecycleEvent;
use crate::review_finding::ReviewFinding;
use crate::rollback::RollbackEvent;
use crate::violation::ViolationEvent;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

/// One active lease for a task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Lease {
    /// The task content-key.
    pub key: String,
    /// Human-readable task title.
    pub title: String,
    /// Session ID of the session that holds this lease.
    pub session_id: String,
    /// Run ID of the run that holds this lease.
    pub run_id: String,
    /// Unix timestamp when the lease was first claimed.
    pub claimed_at: i64,
    /// Unix timestamp of last heartbeat (liveness signal).
    pub heartbeat_at: i64,
}

/// The registry of all active leases: key -> Lease.
pub type LeaseRegistry = BTreeMap<String, Lease>;

/// TTL for lease staleness in seconds (30 minutes).
pub const LEASE_TTL_SECS: i64 = 1800;

/// Resolve the storage root directory: `~/.overwatch/<project-key>/overwatch/`
fn storage_root(cwd: &Path) -> Result<PathBuf> {
    let base = harness_core::config::base_dir("overwatch");
    let repo_root = harness_core::projkey::repo_root(cwd);
    let project_key = harness_core::projkey::project_key(&repo_root);
    Ok(base.join(&project_key).join("overwatch"))
}

/// Path to the leases.json file.
pub fn leases_path(cwd: &Path) -> Result<PathBuf> {
    Ok(storage_root(cwd)?.join("leases.json"))
}

/// Path to the events.jsonl file (append-only).
pub fn events_path(cwd: &Path) -> Result<PathBuf> {
    Ok(storage_root(cwd)?.join("events.jsonl"))
}

/// Path to the violations.jsonl file (append-only, gate-violation events for
/// fleet-level correlated-error detection). Kept as a separate stream from
/// events.jsonl since violations are a distinct signal (cross-task recurrence
/// aggregation) from the lease lifecycle log.
pub fn violations_path(cwd: &Path) -> Result<PathBuf> {
    Ok(storage_root(cwd)?.join("violations.jsonl"))
}

/// Fail-soft load: a missing or corrupt leases.json is treated as an empty registry.
pub fn load_leases(cwd: &Path) -> Result<LeaseRegistry> {
    let path = leases_path(cwd)?;
    match std::fs::read_to_string(&path) {
        Ok(txt) => Ok(serde_json::from_str(&txt).unwrap_or_default()),
        Err(_) => Ok(LeaseRegistry::default()),
    }
}

/// Atomic write (temp + rename) of the lease registry.
pub fn save_leases(cwd: &Path, leases: &LeaseRegistry) -> Result<()> {
    let path = leases_path(cwd)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(leases)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &json)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Append a lifecycle event to the events.jsonl file (one JSON line per event).
pub fn append_event(cwd: &Path, event: &LifecycleEvent) -> Result<()> {
    let path = events_path(cwd)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string(event)?;
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?
        .write_all(format!("{}\n", json).as_bytes())?;
    Ok(())
}

/// Read all events from events.jsonl. Returns an empty vec if the file doesn't exist or is empty.
pub fn read_events(cwd: &Path) -> Result<Vec<LifecycleEvent>> {
    let path = events_path(cwd)?;
    match std::fs::read_to_string(&path) {
        Ok(txt) => {
            let mut events = Vec::new();
            for line in txt.lines() {
                if !line.is_empty() {
                    if let Ok(event) = serde_json::from_str::<LifecycleEvent>(line) {
                        events.push(event);
                    }
                }
            }
            Ok(events)
        }
        Err(_) => Ok(Vec::new()),
    }
}

/// A signature is bucketable (safe to correlate on) only if it has a
/// non-empty discriminator after the `<source>:` prefix. An empty tail (or an
/// empty/whitespace-only signature) is a catch-all that would merge unrelated
/// failures, so it is rejected.
fn signature_is_bucketable(signature: &str) -> bool {
    match signature.split_once(':') {
        // `<source>:<discriminator>` — discriminator must be non-blank.
        Some((source, discriminator)) => {
            !source.trim().is_empty() && !discriminator.trim().is_empty()
        }
        // No `:` at all is malformed / not a real signature.
        None => false,
    }
}

/// Append a gate-violation event to the violations.jsonl file (one JSON line
/// per event), for fleet-level correlated-error detection.
///
/// An event whose signature has no discriminator (shape `<source>:` with an
/// empty tail, or an empty/whitespace-only signature) is **not** persisted:
/// such a signature carries no information to distinguish one failure from
/// another, so recording it would merge unrelated failures into one bucket and
/// falsely flag them as systemic. This is a fail-soft backstop — the normal
/// path via `violation::build_event` already drops un-bucketable events
/// (returns `None`) before reaching here — but it guards any caller that
/// hand-builds a `ViolationEvent`. Returns `Ok(())` without writing (skip).
pub fn append_violation(cwd: &Path, event: &ViolationEvent) -> Result<()> {
    if !signature_is_bucketable(&event.signature) {
        return Ok(());
    }
    let path = violations_path(cwd)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string(event)?;
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?
        .write_all(format!("{}\n", json).as_bytes())?;
    Ok(())
}

/// Read all gate-violation events from violations.jsonl. Returns an empty
/// vec if the file doesn't exist or is empty (fail-soft, same contract as
/// `read_events`).
pub fn read_violations(cwd: &Path) -> Result<Vec<ViolationEvent>> {
    let path = violations_path(cwd)?;
    match std::fs::read_to_string(&path) {
        Ok(txt) => {
            let mut events = Vec::new();
            for line in txt.lines() {
                if !line.is_empty() {
                    if let Ok(event) = serde_json::from_str::<ViolationEvent>(line) {
                        events.push(event);
                    }
                }
            }
            Ok(events)
        }
        Err(_) => Ok(Vec::new()),
    }
}

/// Path to the rollbacks.jsonl file (append-only, canary rollback events).
/// Kept as its own stream since a rollback is a distinct signal (a deploy-time
/// health-gate action) from both the lease lifecycle log and the gate-violation
/// ledger.
pub fn rollbacks_path(cwd: &Path) -> Result<PathBuf> {
    Ok(storage_root(cwd)?.join("rollbacks.jsonl"))
}

/// Append a canary rollback event to rollbacks.jsonl (one JSON line per event).
/// Fail-soft by contract at the call site: emission must never break a rollout.
pub fn append_rollback(cwd: &Path, event: &RollbackEvent) -> Result<()> {
    let path = rollbacks_path(cwd)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string(event)?;
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?
        .write_all(format!("{}\n", json).as_bytes())?;
    Ok(())
}

/// Read all canary rollback events from rollbacks.jsonl. Returns an empty vec
/// if the file doesn't exist or is empty (fail-soft, same contract as
/// `read_events` / `read_violations`).
pub fn read_rollbacks(cwd: &Path) -> Result<Vec<RollbackEvent>> {
    let path = rollbacks_path(cwd)?;
    match std::fs::read_to_string(&path) {
        Ok(txt) => {
            let mut events = Vec::new();
            for line in txt.lines() {
                if !line.is_empty() {
                    if let Ok(event) = serde_json::from_str::<RollbackEvent>(line) {
                        events.push(event);
                    }
                }
            }
            Ok(events)
        }
        Err(_) => Ok(Vec::new()),
    }
}

/// Path to the review_findings.jsonl file (append-only, AI/adversarial review
/// findings). Its own stream since a review finding is a distinct signal from
/// violations/rollbacks; today there is no producer (see `review_finding.rs`),
/// so this file is normally absent and reads fail-soft to empty.
pub fn review_findings_path(cwd: &Path) -> Result<PathBuf> {
    Ok(storage_root(cwd)?.join("review_findings.jsonl"))
}

/// Append an AI-review finding to review_findings.jsonl (one JSON line each).
pub fn append_review_finding(cwd: &Path, finding: &ReviewFinding) -> Result<()> {
    let path = review_findings_path(cwd)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string(finding)?;
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?
        .write_all(format!("{}\n", json).as_bytes())?;
    Ok(())
}

/// Read all AI-review findings from review_findings.jsonl. Returns an empty vec
/// if the file doesn't exist or is empty (fail-soft): with no producer wired
/// yet, this is the normal case and the review-queue degrades gracefully.
pub fn read_review_findings(cwd: &Path) -> Result<Vec<ReviewFinding>> {
    let path = review_findings_path(cwd)?;
    match std::fs::read_to_string(&path) {
        Ok(txt) => {
            let mut findings = Vec::new();
            for line in txt.lines() {
                if !line.is_empty() {
                    if let Ok(f) = serde_json::from_str::<ReviewFinding>(line) {
                        findings.push(f);
                    }
                }
            }
            Ok(findings)
        }
        Err(_) => Ok(Vec::new()),
    }
}

/// Check if a key is held by a live OTHER session (different session_id).
/// Returns true if a lease exists for the key AND it's held by a different session.
pub fn is_held_by_other(leases: &LeaseRegistry, key: &str, session_id: &str, now: i64) -> bool {
    if let Some(lease) = leases.get(key) {
        !is_stale(lease, now) && lease.session_id != session_id
    } else {
        false
    }
}

/// Check if a lease is stale (heartbeat older than TTL).
pub fn is_stale(lease: &Lease, now: i64) -> bool {
    now.saturating_sub(lease.heartbeat_at) > LEASE_TTL_SECS
}

/// Reap stale leases from the registry (in-place mutation).
pub fn reap_stale(leases: &mut LeaseRegistry, now: i64) {
    leases.retain(|_, lease| !is_stale(lease, now));
}

/// Get current time as unix seconds.
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn init() -> Result<()> {
    // Placeholder: actual initialization is done on-demand by load/save.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_bucketable_rejects_empty_discriminator() {
        assert!(signature_is_bucketable("blastguard:rm-rf"));
        assert!(signature_is_bucketable(
            "specguard:spec-without-impl:crate::foo"
        ));
        // Empty / blank discriminator -> not bucketable.
        assert!(!signature_is_bucketable("blastguard:"));
        assert!(!signature_is_bucketable("blastguard:   "));
        // Empty / malformed signature -> not bucketable.
        assert!(!signature_is_bucketable(""));
        assert!(!signature_is_bucketable("blastguard"));
        assert!(!signature_is_bucketable(":rm-rf"));
    }

    #[test]
    fn is_held_by_other_detects_different_session() {
        let mut leases = LeaseRegistry::new();
        leases.insert(
            "key1".to_string(),
            Lease {
                key: "key1".to_string(),
                title: "task".to_string(),
                session_id: "session-a".to_string(),
                run_id: "run-1".to_string(),
                claimed_at: 100,
                heartbeat_at: 100,
            },
        );

        // Different session → held by other
        assert!(is_held_by_other(&leases, "key1", "session-b", 100));
        // Same session → NOT held by other
        assert!(!is_held_by_other(&leases, "key1", "session-a", 100));
        // Non-existent key → NOT held by other
        assert!(!is_held_by_other(&leases, "key2", "session-a", 100));
    }

    #[test]
    fn is_stale_detects_old_heartbeat() {
        let lease = Lease {
            key: "k".to_string(),
            title: "t".to_string(),
            session_id: "s".to_string(),
            run_id: "r".to_string(),
            claimed_at: 0,
            heartbeat_at: 100,
        };

        // Within TTL → not stale
        assert!(!is_stale(&lease, 100 + 1000));
        // Past TTL → stale
        assert!(is_stale(&lease, 100 + LEASE_TTL_SECS + 1));
    }

    #[test]
    fn reap_stale_removes_old_leases() {
        let mut leases = LeaseRegistry::new();
        leases.insert(
            "fresh".to_string(),
            Lease {
                key: "fresh".to_string(),
                title: "t1".to_string(),
                session_id: "s1".to_string(),
                run_id: "r1".to_string(),
                claimed_at: 0,
                heartbeat_at: 1000,
            },
        );
        leases.insert(
            "stale".to_string(),
            Lease {
                key: "stale".to_string(),
                title: "t2".to_string(),
                session_id: "s2".to_string(),
                run_id: "r2".to_string(),
                claimed_at: 0,
                heartbeat_at: 0,
            },
        );

        // "fresh" heartbeat is at 1000, so it's stale at 1000 + TTL + 1
        // "stale" heartbeat is at 0, so it's stale even earlier
        // Use now = 2000, which is past stale (0 + TTL < 2000) but within fresh (1000 + TTL > 2000)
        let now = 1000 + (LEASE_TTL_SECS / 2);
        reap_stale(&mut leases, now);

        assert!(leases.contains_key("fresh"));
        assert!(!leases.contains_key("stale"));
        assert_eq!(leases.len(), 1);
    }
}
