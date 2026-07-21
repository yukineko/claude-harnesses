/// Event store and lease storage backend.
use crate::audit_round::AuditRound;
use crate::changeset::{
    detect_conflicts_default, ActualChangeset, ChangesetRegistry, RuntimeConflictEvent,
};
use crate::disposition::Disposition;
use crate::event::LifecycleEvent;
use crate::lock::LeaseLock;
use crate::merge_conflict::{MergeConflictEntry, MergeConflictResolution};
use crate::review_finding::ReviewFinding;
use crate::rollback::RollbackEvent;
use crate::violation::ViolationEvent;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

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
    /// Files / globs this session is responsible for (PDO session anchor,
    /// DESIGN §4.1). Empty = scope not yet fixed (investigation / carve loop);
    /// such leases are excluded from scope-overlap checks. `#[serde(default)]`
    /// keeps pre-existing leases.json (without this field) readable.
    #[serde(default)]
    pub scope: Vec<String>,
    /// This session's definition of "done" (compass/condukt vocabulary). Used to
    /// re-anchor the session's own memory (§4.3). `#[serde(default)]` keeps old
    /// leases.json readable.
    #[serde(default)]
    pub done_criteria: Option<String>,
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

/// Path to the status_cache.json file: a short-lived cache of the rendered
/// `ProgressView` (see `aggregate::build_cached`), used to collapse the
/// SessionStart+Stop hook double-scan (each `overwatch status` invocation
/// otherwise re-spawns ~5 subprocesses). Its own file since it is a derived,
/// disposable cache — not part of the append-only event/lease ledgers, and
/// safe to delete or corrupt without losing any signal (a miss just falls
/// back to a fresh `aggregate::build`).
pub fn status_cache_path(cwd: &Path) -> Result<PathBuf> {
    Ok(storage_root(cwd)?.join("status_cache.json"))
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
    let tmp = unique_tmp(&path, "json");
    std::fs::write(&tmp, &json)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Process-wide monotonic counter so two rewrites in the SAME process (identical
/// pid, and possibly an identical `now_unix_nanos()` under a coarse clock) never
/// derive the same temp name. Without it a nanos collision between two concurrent
/// rewrites of one store could have both publish over the SAME temp file, so a
/// reader observes a half-written rename source. Mirrors `lock::TMP_SEQ`.
static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

fn now_unix_nanos() -> u128 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// Build a per-call-unique sibling temp path for an atomic rewrite:
/// `<stem>.<tag>.tmp.<pid>.<nanos>.<seq>`. Two concurrent rewrites of the same
/// store therefore never collide on a single temp name (which would let one
/// publish a half-written file the other renames into place), mirroring
/// `lock.rs`'s `TMP_SEQ`/`now_unix_nanos` idiom. Replaces the fixed
/// `path.with_extension("jsonl.tmp")` used by the atomic rewrite helpers.
fn unique_tmp(path: &Path, tag: &str) -> PathBuf {
    path.with_extension(format!(
        "{tag}.tmp.{}.{}.{}",
        std::process::id(),
        now_unix_nanos(),
        TMP_SEQ.fetch_add(1, Ordering::Relaxed)
    ))
}

/// Sleep for the millisecond count named by env var `var`, if set and parseable;
/// a no-op otherwise. Used ONLY to widen a read->rewrite / check->append race
/// window in the concurrency regression tests (never set in production),
/// mirroring `lease::artificial_delay`.
fn artificial_delay(var: &str) {
    if let Some(ms) = std::env::var(var).ok().and_then(|s| s.parse::<u64>().ok()) {
        std::thread::sleep(std::time::Duration::from_millis(ms));
    }
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

/// Three-valued result of reading the violation registry for a decision that
/// must FAIL CLOSED on an undetermined store (the canary health gate). Unlike
/// [`read_violations`] — which is fail-soft by contract for display/observational
/// callers (any read/parse trouble collapses to an empty vec) — this scan keeps
/// "genuinely no violations yet" DISTINCT from "cannot be trusted", so a
/// fleet-defense caller can hold instead of reading a broken store as clean.
#[derive(Debug)]
pub enum ViolationScan {
    /// The registry file does not exist: no violation has ever been recorded
    /// for this project. A legitimately-empty history — safe to treat as zero
    /// violations (this is the normal first-deploy case, so it must NOT trip a
    /// rollback).
    Absent,
    /// The count cannot be trusted: the file exists but could not be read
    /// (I/O / permission error), OR it was read but at least one non-empty line
    /// failed to parse (schema drift / corruption) — meaning a real violation
    /// could be silently dropped, under-counting the health signal. A caller
    /// that gates the fleet must fail CLOSED here (hold / roll back), never read
    /// this as "no violations".
    Undetermined,
    /// The file was read cleanly and EVERY non-empty line parsed: this is the
    /// authoritative, trustworthy violation list (possibly empty if the file
    /// existed but held only blank lines).
    Events(Vec<ViolationEvent>),
}

/// Strictly scan the violation registry, distinguishing "absent" (legit empty)
/// from "undetermined" (unreadable / partially-unparseable → untrustworthy)
/// from a clean, fully-parsed event list. This is the fail-CLOSED counterpart
/// to [`read_violations`]: use it only where an undetermined store must block
/// (the canary health gate), so a broken/corrupt store is never silently read
/// as "zero violations → proceed". A single non-empty line that fails to parse
/// makes the WHOLE scan `Undetermined` — a schema-drifted line may be a real
/// violation we can no longer see, so the count is untrustworthy.
pub fn scan_violations(cwd: &Path) -> ViolationScan {
    let path = match violations_path(cwd) {
        Ok(p) => p,
        // Cannot even resolve the storage path (e.g. no HOME): treat as
        // undetermined rather than empty — we cannot claim "no violations".
        Err(_) => return ViolationScan::Undetermined,
    };
    match std::fs::read_to_string(&path) {
        Ok(txt) => {
            let mut events = Vec::new();
            for line in txt.lines() {
                if line.is_empty() {
                    continue;
                }
                match serde_json::from_str::<ViolationEvent>(line) {
                    Ok(event) => events.push(event),
                    // A present-but-unparseable line means the store is
                    // schema-drifted/corrupt; we cannot trust the count.
                    Err(_) => return ViolationScan::Undetermined,
                }
            }
            ViolationScan::Events(events)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => ViolationScan::Absent,
        // File exists but is unreadable (permission / I/O): undetermined, not
        // empty. This is exactly the fail-open the canary gate must avoid.
        Err(_) => ViolationScan::Undetermined,
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

/// Record one AI/adversarial review finding into the overwatch-readable store,
/// stamping the current timestamp. Thin library entry point so an external
/// crate (e.g. condukt's gate-exec escalate path — the first real producer of
/// this stream) can record a finding by value without hand-constructing a
/// [`ReviewFinding`] or reaching into private fields. Mirrors
/// [`append_review_finding`]; callers that need fail-soft semantics (a
/// recording failure must never change their own return value) should ignore
/// the `Err` the way `append_review_finding` callers already do.
#[allow(clippy::too_many_arguments)]
pub fn record_finding(
    cwd: &Path,
    finding_id: String,
    source: String,
    severity: Option<String>,
    summary: String,
    file: Option<String>,
    rationale: Option<String>,
) -> Result<()> {
    let finding = ReviewFinding::new(
        finding_id,
        source,
        severity,
        summary,
        file,
        rationale,
        now(),
    );
    append_review_finding(cwd, &finding)
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

/// One recorded bridge event: a finding-id that has already been forwarded to
/// the backlog by `review-queue --to-backlog`. The stream is the idempotency
/// key set — the backlog's own duplicate guard hashes on title+project, not on
/// finding-id, so cross-round idempotency (the same finding re-recorded every
/// audit round) is enforced here by finding-id.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgedFinding {
    /// The finding-id that was forwarded to the backlog.
    pub finding_id: String,
    /// Unix timestamp when the bridge happened.
    pub ts: i64,
}

/// Path to the bridged_findings.jsonl file (append-only): the set of finding-ids
/// already forwarded to the backlog, used to make `review-queue --to-backlog`
/// idempotent across audit rounds.
pub fn bridged_findings_path(cwd: &Path) -> Result<PathBuf> {
    Ok(storage_root(cwd)?.join("bridged_findings.jsonl"))
}

/// Append a bridged-finding record to bridged_findings.jsonl (one JSON line
/// each). Called after a successful `backlog add` so the finding is never
/// forwarded twice.
pub fn append_bridged_finding(cwd: &Path, finding_id: &str) -> Result<()> {
    let path = bridged_findings_path(cwd)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Hold the store LeaseLock across a check-then-append so two concurrent
    // `review-queue --to-backlog` runs cannot both observe the finding-id as
    // absent and double-add it (check-then-append TOCTOU). Idempotent: re-check
    // membership INSIDE the lock and skip if already present. Guarding it here
    // (the shared append site) covers ALL callers. HARD-SKIP on contention: skip
    // the append rather than proceed to an unlocked check-then-append that could
    // double-forward the finding. The old fail-soft `acquire` left that window
    // open.
    let _lock = match LeaseLock::acquire_or_skip(cwd) {
        Some(l) => l,
        None => return Ok(()),
    };
    if read_bridged_findings(cwd)?
        .iter()
        .any(|id| id == finding_id)
    {
        return Ok(());
    }
    // Test-only race widener (no-op in prod).
    artificial_delay("OVERWATCH_TEST_BRIDGE_DELAY_MS");
    let rec = BridgedFinding {
        finding_id: finding_id.to_string(),
        ts: now(),
    };
    let json = serde_json::to_string(&rec)?;
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?
        .write_all(format!("{}\n", json).as_bytes())?;
    Ok(())
}

/// Read the set of already-bridged finding-ids from bridged_findings.jsonl.
/// Returns an empty vec if the file doesn't exist or is empty (fail-soft, same
/// contract as `read_review_findings`). Corrupt lines are skipped.
pub fn read_bridged_findings(cwd: &Path) -> Result<Vec<String>> {
    let path = bridged_findings_path(cwd)?;
    match std::fs::read_to_string(&path) {
        Ok(txt) => {
            let mut ids = Vec::new();
            for line in txt.lines() {
                if !line.is_empty() {
                    if let Ok(r) = serde_json::from_str::<BridgedFinding>(line) {
                        ids.push(r.finding_id);
                    }
                }
            }
            Ok(ids)
        }
        Err(_) => Ok(Vec::new()),
    }
}

/// One record in the bridged-entries ledger: a non-finding review-queue entry
/// (systemic / rollback / escalation) that has already been forwarded to the
/// backlog. Kept in a SEPARATE file from [`BridgedFinding`] on purpose — the
/// finding ledger (`bridged_findings.jsonl`) doubles as the review-metrics
/// "resolved finding-id" source (see [`compact_review_findings`]), so its
/// entries must stay bare finding-ids. The other three streams get their own
/// composite-key ledger here rather than polluting that one.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgedEntry {
    /// The idempotency key `<kind-tag>:<identifier>` that was forwarded.
    pub key: String,
    /// Unix timestamp when the bridge happened.
    pub ts: i64,
}

/// Path to the bridged_entries.jsonl file (append-only): the set of composite
/// `<kind-tag>:<identifier>` keys for non-finding review-queue entries already
/// forwarded to the backlog, making that half of `review-queue --to-backlog`
/// idempotent across runs. Distinct from [`bridged_findings_path`] so the
/// finding-resolution logic that reads that file is unaffected.
pub fn bridged_entries_path(cwd: &Path) -> Result<PathBuf> {
    Ok(storage_root(cwd)?.join("bridged_entries.jsonl"))
}

/// Append a bridged-entry record to bridged_entries.jsonl (one JSON line each).
/// Called after a successful `backlog add` for a non-finding stream so the
/// entry is never forwarded twice.
pub fn append_bridged_entry(cwd: &Path, key: &str) -> Result<()> {
    let path = bridged_entries_path(cwd)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Same check-then-append TOCTOU guard as `append_bridged_finding`: hold the
    // store LeaseLock and re-check membership inside it so two concurrent
    // to-backlog runs cannot double-add the same `<kind>:<identifier>` key.
    // HARD-SKIP on contention: skip the append rather than proceed to an
    // unlocked check-then-append that could double-add. The old fail-soft
    // `acquire` left that window open.
    let _lock = match LeaseLock::acquire_or_skip(cwd) {
        Some(l) => l,
        None => return Ok(()),
    };
    if read_bridged_entries(cwd)?.iter().any(|k| k == key) {
        return Ok(());
    }
    // Test-only race widener (no-op in prod).
    artificial_delay("OVERWATCH_TEST_BRIDGE_DELAY_MS");
    let rec = BridgedEntry {
        key: key.to_string(),
        ts: now(),
    };
    let json = serde_json::to_string(&rec)?;
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?
        .write_all(format!("{}\n", json).as_bytes())?;
    Ok(())
}

/// Read the set of already-bridged non-finding entry keys from
/// bridged_entries.jsonl. Empty vec if the file is absent/empty (fail-soft,
/// same contract as [`read_bridged_findings`]). Corrupt lines are skipped.
pub fn read_bridged_entries(cwd: &Path) -> Result<Vec<String>> {
    let path = bridged_entries_path(cwd)?;
    match std::fs::read_to_string(&path) {
        Ok(txt) => {
            let mut keys = Vec::new();
            for line in txt.lines() {
                if !line.is_empty() {
                    if let Ok(r) = serde_json::from_str::<BridgedEntry>(line) {
                        keys.push(r.key);
                    }
                }
            }
            Ok(keys)
        }
        Err(_) => Ok(Vec::new()),
    }
}

/// Path to the audit_rounds.jsonl file (append-only, Continuous-Audit round
/// metrics). Its own stream since a round record is a distinct signal from the
/// findings/rollback/violation logs: it is the convergence ledger the
/// Continuous-Audit loop reads back via `overwatch audit-metrics`.
pub fn audit_rounds_path(cwd: &Path) -> Result<PathBuf> {
    Ok(storage_root(cwd)?.join("audit_rounds.jsonl"))
}

/// Append a Continuous-Audit round record to audit_rounds.jsonl (one JSON line
/// per round). Fail-soft by contract at the call site: recording a round must
/// never break the audit loop.
pub fn append_audit_round(cwd: &Path, round: &AuditRound) -> Result<()> {
    let path = audit_rounds_path(cwd)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string(round)?;
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?
        .write_all(format!("{}\n", json).as_bytes())?;
    Ok(())
}

/// Rewrite audit_rounds.jsonl from `rounds` (one JSON line each, in the given
/// order). This is the persistence half of the closure-feedback path
/// (`audit-round close`): the caller reads the ledger, updates a round's
/// `regression_tests_added` in memory via [`crate::audit_round::set_round_tests`],
/// then calls this to write it back. Writes to a sibling temp file and renames so
/// a crash mid-write cannot leave a truncated ledger. Fail-soft by contract at
/// the call site (a write error is reported, never propagated to break a turn).
pub fn rewrite_audit_rounds(cwd: &Path, rounds: &[AuditRound]) -> Result<()> {
    let path = audit_rounds_path(cwd)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut buf = String::new();
    for r in rounds {
        buf.push_str(&serde_json::to_string(r)?);
        buf.push('\n');
    }
    // Unique temp per call so two concurrent rewrites cannot collide on one temp
    // and publish a half-written ledger. The caller (audit_round_cli::close)
    // holds the store LeaseLock across its read->modify->rewrite so a concurrent
    // writer serializes rather than having its append clobbered.
    let tmp = unique_tmp(&path, "jsonl");
    std::fs::write(&tmp, buf.as_bytes())?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Read all Continuous-Audit round records from audit_rounds.jsonl, in recorded
/// (append) order. Returns an empty vec if the file doesn't exist or is empty
/// (fail-soft, same contract as `read_events` / `read_rollbacks`). Corrupt
/// lines are skipped rather than failing the whole read.
pub fn read_audit_rounds(cwd: &Path) -> Result<Vec<AuditRound>> {
    let path = audit_rounds_path(cwd)?;
    match std::fs::read_to_string(&path) {
        Ok(txt) => {
            let mut rounds = Vec::new();
            for line in txt.lines() {
                if !line.is_empty() {
                    if let Ok(r) = serde_json::from_str::<AuditRound>(line) {
                        rounds.push(r);
                    }
                }
            }
            Ok(rounds)
        }
        Err(_) => Ok(Vec::new()),
    }
}

/// Path to the dispositions.jsonl file (append-only, human dispositions of
/// AI/adversarial review findings — review-effectiveness measurement). Its
/// own stream since a disposition is a distinct signal from the findings
/// themselves (see `disposition.rs`); `overwatch review-metrics` reads it
/// back joined against `review_findings.jsonl`.
pub fn dispositions_path(cwd: &Path) -> Result<PathBuf> {
    Ok(storage_root(cwd)?.join("dispositions.jsonl"))
}

/// Outcome of a `LeaseLock`-guarded check-then-append store write. Lets a CLI
/// surface report a contended HARD-SKIP TRUTHFULLY instead of a phantom
/// success: a contended append persists NOTHING, yet used to return a bare
/// `Ok(())` indistinguishable from a real write, so callers printed
/// `recorded:true` / `resolved:true` while the ledger was untouched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppendOutcome {
    /// The record is persisted: written this call, or already present
    /// (idempotent dedup on the join key). Truthful `recorded:true`.
    Recorded,
    /// The store `LeaseLock` was contended past its deadline; the append was
    /// SKIPPED and nothing was persisted this call. The caller should report
    /// `recorded:false` (nonzero exit) and retry shortly.
    SkippedContended,
}

/// Append a human disposition to dispositions.jsonl (one JSON line each).
///
/// Idempotent per `finding_id`: two concurrent writers (e.g. a `reconcile-fixed`
/// run and a manual `record-disposition`, or two reconcile runs) that both
/// resolve the SAME finding_id must not double-row it. Guarding it HERE — the
/// shared write site both callers funnel through — covers all callers
/// automatically. Same check-then-append TOCTOU guard as
/// [`append_bridged_finding`]: hold the store `LeaseLock` across the
/// read->check->append critical section and re-check for an existing
/// disposition with this `finding_id` INSIDE the lock, skipping the append if
/// one is already present. Fail-soft: `LeaseLock` degrades to unlocked on
/// timeout and never panics; a corrupt existing line is skipped by
/// [`read_dispositions`].
pub fn append_disposition(cwd: &Path, disposition: &Disposition) -> Result<AppendOutcome> {
    append_disposition_with_deadline(cwd, disposition, LeaseLock::DEADLINE)
}

/// Deadline-parameterized core of [`append_disposition`] (production passes the
/// 10s default; the wedged-holder regression test passes a short deadline to
/// drive the same skip-on-contention path fast). HARD-SKIP semantics: if the
/// store lock cannot be acquired within `deadline` we SKIP the append entirely
/// rather than proceed to an unlocked check-then-append that could double-row
/// the disposition. The old fail-soft `acquire` handed back an UNLOCKED guard
/// and let the append proceed — the exact TOCTOU this closes.
///
/// Returns an [`AppendOutcome`] so the CLI surface can distinguish a genuine
/// persist / idempotent dedup ([`AppendOutcome::Recorded`]) from a contended
/// HARD-SKIP ([`AppendOutcome::SkippedContended`], nothing persisted) and
/// report it truthfully — a contended skip previously returned a bare `Ok(())`
/// indistinguishable from success, so `record-disposition` printed
/// `recorded:true` while NOTHING was written.
fn append_disposition_with_deadline(
    cwd: &Path,
    disposition: &Disposition,
    deadline: std::time::Duration,
) -> Result<AppendOutcome> {
    let path = dispositions_path(cwd)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _lock = match LeaseLock::acquire_or_skip_with_deadline(cwd, deadline) {
        Some(l) => l,
        None => return Ok(AppendOutcome::SkippedContended),
    };
    if read_dispositions(cwd)?
        .iter()
        .any(|d| d.finding_id == disposition.finding_id)
    {
        // Already present (idempotent dedup on finding_id): the disposition IS
        // persisted, so this is a truthful Recorded, not a skip.
        return Ok(AppendOutcome::Recorded);
    }
    // Test-only race widener (no-op in prod).
    artificial_delay("OVERWATCH_TEST_DISPOSITION_DELAY_MS");
    let json = serde_json::to_string(disposition)?;
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?
        .write_all(format!("{}\n", json).as_bytes())?;
    Ok(AppendOutcome::Recorded)
}

/// Read all dispositions from dispositions.jsonl. Returns an empty vec if the
/// file doesn't exist or is empty (fail-soft, same contract as
/// `read_review_findings`). Corrupt lines are skipped rather than failing the
/// whole read.
pub fn read_dispositions(cwd: &Path) -> Result<Vec<Disposition>> {
    let path = dispositions_path(cwd)?;
    match std::fs::read_to_string(&path) {
        Ok(txt) => {
            let mut dispositions = Vec::new();
            for line in txt.lines() {
                if !line.is_empty() {
                    if let Ok(d) = serde_json::from_str::<Disposition>(line) {
                        dispositions.push(d);
                    }
                }
            }
            Ok(dispositions)
        }
        Err(_) => Ok(Vec::new()),
    }
}

// ── Mid-flight runtime-conflict detection (design 625aa170 A) ────────────────

/// Path to `active_changesets.json`: the project-global registry of in-flight
/// ACTUAL changesets (`task_key -> ActualChangeset`), a mutable JSON map
/// (temp+rename atomic write) like `leases.json`. Distinct stream from the
/// append-only ledgers since it is a live, upsert-in-place set.
pub fn active_changesets_path(cwd: &Path) -> Result<PathBuf> {
    Ok(storage_root(cwd)?.join("active_changesets.json"))
}

/// Path to `runtime_conflicts.jsonl`: the append-only ledger of detected
/// mid-flight overlaps (one `RuntimeConflictEvent` per overlapping pair).
pub fn runtime_conflicts_path(cwd: &Path) -> Result<PathBuf> {
    Ok(storage_root(cwd)?.join("runtime_conflicts.jsonl"))
}

/// Fail-soft load of the active-changeset registry: a missing or corrupt file
/// is treated as an empty registry (same contract as `load_leases`).
pub fn load_active_changesets(cwd: &Path) -> Result<ChangesetRegistry> {
    let path = active_changesets_path(cwd)?;
    match std::fs::read_to_string(&path) {
        Ok(txt) => Ok(serde_json::from_str(&txt).unwrap_or_default()),
        Err(_) => Ok(ChangesetRegistry::default()),
    }
}

/// Atomic write (unique temp + rename) of the active-changeset registry.
pub fn save_active_changesets(cwd: &Path, registry: &ChangesetRegistry) -> Result<()> {
    let path = active_changesets_path(cwd)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(registry)?;
    let tmp = unique_tmp(&path, "json");
    std::fs::write(&tmp, &json)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Append one runtime-conflict event to `runtime_conflicts.jsonl` (one JSON
/// line each). Fail-soft by contract at the call site (detection must never
/// break a turn).
pub fn append_runtime_conflict(cwd: &Path, event: &RuntimeConflictEvent) -> Result<()> {
    let path = runtime_conflicts_path(cwd)?;
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

/// Read all runtime-conflict events (fail-soft, corrupt lines skipped).
pub fn read_runtime_conflicts(cwd: &Path) -> Result<Vec<RuntimeConflictEvent>> {
    let path = runtime_conflicts_path(cwd)?;
    match std::fs::read_to_string(&path) {
        Ok(txt) => {
            let mut events = Vec::new();
            for line in txt.lines() {
                if !line.is_empty() {
                    if let Ok(ev) = serde_json::from_str::<RuntimeConflictEvent>(line) {
                        events.push(ev);
                    }
                }
            }
            Ok(events)
        }
        Err(_) => Ok(Vec::new()),
    }
}

/// Record `changeset` into the project-global registry AND cross-check it
/// against every OTHER in-flight entry, returning the detected overlaps.
///
/// This is the shared-registry read-modify-write that decision A gates on. It
/// runs under the overwatch `LeaseLock` (the SAME lock every other overwatch
/// registry mutation takes) so two tasks recording concurrently serialize
/// rather than both loading the same pre-record snapshot and losing one
/// upsert (a lost changeset would silently miss a real overlap). Inside the
/// lock: load registry -> detect conflicts vs live `!merged` peers -> append
/// each event -> upsert this changeset -> save. Returns the events so the
/// caller (condukt's detection hook) can set a merge-hold when non-empty.
///
/// Fail-soft on the detection side: the caller wraps this so a lock timeout /
/// write error degrades to "no overlap detected" and never holds a merge on a
/// compute error (only a POSITIVE detection holds). The `LeaseLock` never
/// panics; a write error propagates as `Err` for the caller to swallow.
pub fn record_changeset_and_detect(
    cwd: &Path,
    changeset: &ActualChangeset,
) -> Result<Vec<RuntimeConflictEvent>> {
    // HARD-SKIP on contention: skip the record+detect rather than proceed to an
    // unlocked load->save that could clobber a peer's changeset. Returning no
    // events matches the caller's documented degrade ("no overlap detected" — a
    // positive detection is the only thing that holds a merge), so a contended
    // pass never spuriously holds. The old fail-soft `acquire` left that window
    // open.
    let _lock = match LeaseLock::acquire_or_skip(cwd) {
        Some(l) => l,
        None => return Ok(Vec::new()),
    };
    // Test-only race widener (no-op in prod): widen the load->save window so the
    // concurrency regression test can reliably interleave two writers.
    artificial_delay("OVERWATCH_TEST_CHANGESET_DELAY_MS");
    let mut registry = load_active_changesets(cwd)?;
    let events = detect_conflicts_default(changeset, &registry);
    for ev in &events {
        // Append each detected overlap; a single append failure must not drop
        // the registry upsert below, so log-and-continue rather than early-return.
        if let Err(e) = append_runtime_conflict(cwd, ev) {
            eprintln!("overwatch: WARNING could not append runtime conflict (continuing): {e}");
        }
    }
    registry.insert(changeset.task_key.clone(), changeset.clone());
    save_active_changesets(cwd, &registry)?;
    Ok(events)
}

/// Mark a task's changeset as merged (landed) in the registry, so it is no
/// longer treated as in-flight for overlap checks. RMW under `LeaseLock`.
/// Fail-soft: an absent entry is a no-op. Called on branch-land cleanup.
pub fn mark_changeset_merged(cwd: &Path, task_key: &str) -> Result<()> {
    // HARD-SKIP on contention: skip rather than run an unlocked RMW that could
    // clobber a peer's changeset. Safe: an unmarked entry ages out via
    // prune_stale_changesets. The old fail-soft `acquire` left that window open.
    let _lock = match LeaseLock::acquire_or_skip(cwd) {
        Some(l) => l,
        None => return Ok(()),
    };
    let mut registry = load_active_changesets(cwd)?;
    if let Some(entry) = registry.get_mut(task_key) {
        entry.merged = true;
        save_active_changesets(cwd, &registry)?;
    }
    Ok(())
}

/// Mark EVERY in-flight changeset whose `branch` matches as merged (landed), so
/// it leaves the overlap-detection set. RMW under `LeaseLock`. Returns how many
/// entries were marked. The merge path only knows the branch (not the
/// `task_key`), so this is the branch-keyed sibling of [`mark_changeset_merged`]
/// and is the production caller that closes finding #1 (a landed task's
/// changeset staying `merged=false` and spuriously holding the next sequential
/// task that touches a common file). Fail-soft by contract at the call site: a
/// cleanup error must never break a merge that already succeeded.
pub fn mark_branch_merged(cwd: &Path, branch: &str) -> Result<usize> {
    // HARD-SKIP on contention: skip (0 marked) rather than run an unlocked RMW
    // that could clobber a peer's changeset. Safe: an unmarked entry ages out
    // via prune_stale_changesets. The old fail-soft `acquire` left that window
    // open.
    let _lock = match LeaseLock::acquire_or_skip(cwd) {
        Some(l) => l,
        None => return Ok(0),
    };
    let mut registry = load_active_changesets(cwd)?;
    let mut marked = 0usize;
    for c in registry.values_mut() {
        if !c.merged && c.branch == branch {
            c.merged = true;
            marked += 1;
        }
    }
    if marked > 0 {
        save_active_changesets(cwd, &registry)?;
    }
    Ok(marked)
}

/// Clear (resolve) any OPEN `RuntimeOverlap` merge-hold recorded against
/// `branch` by writing a `Theirs`/`Policy` resolution for each — the branch has
/// landed, so its content won and a lingering hold recorded against the same
/// branch NAME must not block a later run's merge (the hold is looked up by
/// branch only, so a REUSED `condukt/<id>` branch name would otherwise inherit a
/// stale hold). Idempotent (resolution append dedups by `conflict_id`). Returns
/// how many holds were cleared. Fail-soft by contract at the call site.
pub fn clear_runtime_overlap_holds(cwd: &Path, branch: &str, now: i64) -> Result<usize> {
    let open = open_merge_conflicts(cwd)?;
    let mut cleared = 0usize;
    for e in open {
        if e.branch == branch
            && matches!(
                e.origin,
                crate::merge_conflict::ConflictOrigin::RuntimeOverlap
            )
        {
            let resolution = MergeConflictResolution {
                conflict_id: e.conflict_id.clone(),
                choice: crate::merge_conflict::ResolveChoice::Theirs,
                decided_by: crate::merge_conflict::DecidedBy::Policy,
                note: Some("auto-cleared: branch landed".to_string()),
                ts: now,
            };
            append_merge_conflict_resolution(cwd, &resolution)?;
            cleared += 1;
        }
    }
    Ok(cleared)
}

/// Prune changesets that are merged OR stale (older than `LEASE_TTL_SECS`
/// relative to `now`), returning how many were removed. RMW under `LeaseLock`.
/// The lease-liveness filter in `detect_conflicts` already ignores these, so
/// pruning is a housekeeping compaction (keeps the registry bounded); a
/// crashed run that left `merged=false` ages out here on any later record/prune.
pub fn prune_stale_changesets(cwd: &Path, now: i64) -> Result<usize> {
    // HARD-SKIP on contention: skip (0 pruned) rather than run an unlocked RMW
    // that could clobber a peer's changeset. Pruning is housekeeping — a skipped
    // pass runs again later. The old fail-soft `acquire` left that window open.
    let _lock = match LeaseLock::acquire_or_skip(cwd) {
        Some(l) => l,
        None => return Ok(0),
    };
    let mut registry = load_active_changesets(cwd)?;
    let before = registry.len();
    registry.retain(|_, c| !c.merged && (now - c.ts) <= LEASE_TTL_SECS);
    let removed = before - registry.len();
    if removed > 0 {
        save_active_changesets(cwd, &registry)?;
    }
    Ok(removed)
}

/// Path to the review_findings_archive.jsonl file (append-only, COLD store):
/// where `compact_review_findings` moves resolved (bridged or dispositioned)
/// records out of the hot `review_findings.jsonl`. Mirrors
/// [`review_findings_path`]. Kept private since the only reader that needs
/// the archive is [`read_review_findings_all`] / [`compact_review_findings`]
/// in this module — `review_queue.rs` / `bridge.rs` intentionally keep
/// reading the hot file only (the bounded-read win).
fn review_findings_archive_path(cwd: &Path) -> Result<PathBuf> {
    Ok(storage_root(cwd)?.join("review_findings_archive.jsonl"))
}

/// Read ALL AI-review findings across both the hot store and the cold
/// archive (hot records first, then archive, each in its own on-disk/append
/// order). This is the full-history view the review-metrics latency join
/// needs (`disposition_cli::metrics`) so that compacting a resolved finding
/// out of the hot file never orphans its disposition. Fail-soft, same
/// contract as [`read_review_findings`]: a missing archive contributes
/// nothing, corrupt lines are skipped.
pub fn read_review_findings_all(cwd: &Path) -> Result<Vec<ReviewFinding>> {
    let mut all = read_review_findings(cwd)?;
    let archive_path = review_findings_archive_path(cwd)?;
    if let Ok(txt) = std::fs::read_to_string(&archive_path) {
        for line in txt.lines() {
            if !line.is_empty() {
                if let Ok(f) = serde_json::from_str::<ReviewFinding>(line) {
                    all.push(f);
                }
            }
        }
    }
    Ok(all)
}

/// Partition `findings` into `(open, archived)`: a record goes to `archived`
/// iff its `finding_id` is a member of `resolved_ids`, else to `open`. BOTH
/// partitions preserve the input (append) order of their surviving records
/// — load-bearing, since `review_queue::dedup_findings` tie-breaks on append
/// order among equal `ts`. Pure: no I/O, no clock, total (never panics),
/// deterministic (same inputs -> identical output).
pub fn partition_findings(
    findings: &[ReviewFinding],
    resolved_ids: &BTreeSet<String>,
) -> (Vec<ReviewFinding>, Vec<ReviewFinding>) {
    let mut open = Vec::new();
    let mut archived = Vec::new();
    for f in findings {
        if resolved_ids.contains(&f.finding_id) {
            archived.push(f.clone());
        } else {
            open.push(f.clone());
        }
    }
    (open, archived)
}

/// Write `records` to `path` as one JSON line each (in the given order),
/// via a sibling temp file + rename so a crash mid-write cannot leave a
/// truncated file. Mirrors [`rewrite_audit_rounds`]'s atomic idiom, factored
/// out here since [`compact_review_findings`] needs it for BOTH the archive
/// and the hot rewrite.
fn write_jsonl_atomic<T: Serialize>(path: &Path, records: &[T]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut buf = String::new();
    for r in records {
        buf.push_str(&serde_json::to_string(r)?);
        buf.push('\n');
    }
    // Unique temp per call (see `unique_tmp`): compaction rewrites BOTH the
    // archive and the hot file, so a fixed temp name could collide with a
    // concurrent rewrite and publish a half-written file.
    let tmp = unique_tmp(path, "jsonl");
    std::fs::write(&tmp, buf.as_bytes())?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Report of one `compact_review_findings` run.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompactionReport {
    /// How many findings remain in the hot store after compaction (OPEN).
    pub open: usize,
    /// How many findings were newly moved to the archive this run.
    pub archived: usize,
    /// How many findings were ALREADY in the archive before this run.
    pub already_archived: usize,
}

/// Compact the review-findings store: move every finding whose `finding_id`
/// has been resolved (bridged to the backlog, per [`read_bridged_findings`],
/// or dispositioned, per [`read_dispositions`]) out of the hot
/// `review_findings.jsonl` into the cold `review_findings_archive.jsonl`.
/// NON-LOSSY by design (see module docs / the brief): records are MOVED, not
/// deleted, so [`read_review_findings_all`] (the review-metrics latency
/// join) keeps seeing them after compaction.
///
/// Crash-safety ordering: the archive is rewritten FIRST, then the hot file.
/// A crash between the two leaves the resolved records in BOTH files (safe —
/// recoverable, and a re-run is idempotent since the hot file still holds
/// them as "open" and gets re-partitioned identically). Both rewrites are
/// atomic (temp file + rename), mirroring [`rewrite_audit_rounds`].
///
/// Fail-soft: a missing hot file is a no-op reporting all-zero counts (never
/// creates files nor errors). Never panics.
pub fn compact_review_findings(cwd: &Path) -> Result<CompactionReport> {
    let hot_path = review_findings_path(cwd)?;
    if !hot_path.exists() {
        return Ok(CompactionReport {
            open: 0,
            archived: 0,
            already_archived: 0,
        });
    }

    // Hold the store LeaseLock across the WHOLE read->rewrite critical section so
    // a finding appended (by a lock-respecting writer) concurrently with this
    // compaction is not silently dropped: without it, this rewrites the hot file
    // from a stale snapshot taken before the append, clobbering it. HARD-SKIP on
    // contention: skip the compaction (all-zero no-op report) rather than run
    // that unlocked rewrite. Compaction is housekeeping and re-runs later; the
    // old fail-soft `acquire` left the clobber window open.
    let _lock = match LeaseLock::acquire_or_skip(cwd) {
        Some(l) => l,
        None => {
            return Ok(CompactionReport {
                open: 0,
                archived: 0,
                already_archived: 0,
            })
        }
    };

    let findings = read_review_findings(cwd)?;

    let mut resolved_ids: BTreeSet<String> = read_bridged_findings(cwd)?.into_iter().collect();
    for d in read_dispositions(cwd)? {
        resolved_ids.insert(d.finding_id);
    }

    let (open, archived) = partition_findings(&findings, &resolved_ids);

    let archive_path = review_findings_archive_path(cwd)?;
    let existing_archive: Vec<ReviewFinding> = match std::fs::read_to_string(&archive_path) {
        Ok(txt) => {
            let mut v = Vec::new();
            for line in txt.lines() {
                if !line.is_empty() {
                    if let Ok(f) = serde_json::from_str::<ReviewFinding>(line) {
                        v.push(f);
                    }
                }
            }
            v
        }
        Err(_) => Vec::new(),
    };
    let already_archived = existing_archive.len();

    let mut new_archive = existing_archive;
    new_archive.extend(archived.iter().cloned());

    // Test-only race widener (no-op in prod): opens the window between the read
    // above and the hot rewrite below so the concurrency regression test can
    // reliably interleave a concurrent append.
    artificial_delay("OVERWATCH_TEST_COMPACT_DELAY_MS");

    // Archive FIRST (crash-safety: a crash before the hot rewrite leaves the
    // resolved records recoverable in both files, and a re-run is idempotent).
    write_jsonl_atomic(&archive_path, &new_archive)?;
    write_jsonl_atomic(&hot_path, &open)?;

    Ok(CompactionReport {
        open: open.len(),
        archived: archived.len(),
        already_archived,
    })
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

// ── Consensus merge-conflict resolution (design 625aa170 B) ──────────────────

/// Path to `merge_conflicts.jsonl`: the append-only ledger of blocked merges
/// (real 3-way conflicts AND gated mid-flight overlaps) awaiting resolution.
pub fn merge_conflicts_path(cwd: &Path) -> Result<PathBuf> {
    Ok(storage_root(cwd)?.join("merge_conflicts.jsonl"))
}

/// Path to `merge_conflict_resolutions.jsonl`: the append-only ledger of
/// resolutions, joined to entries by `conflict_id` (mirrors dispositions).
pub fn merge_conflict_resolutions_path(cwd: &Path) -> Result<PathBuf> {
    Ok(storage_root(cwd)?.join("merge_conflict_resolutions.jsonl"))
}

/// Append one blocked-merge entry to `merge_conflicts.jsonl` (one JSON line
/// each). Idempotent per `conflict_id`: re-recording the same blocked merge (a
/// re-run of the same held task) is a no-op, guarded by a check-then-append
/// under `LeaseLock` (same TOCTOU guard as `append_disposition`). Fail-soft by
/// contract at the call site (recording must never break a turn).
pub fn append_merge_conflict(cwd: &Path, entry: &MergeConflictEntry) -> Result<()> {
    let path = merge_conflicts_path(cwd)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // HARD-SKIP on contention: skip the append rather than proceed to an
    // unlocked check-then-append that could double-row the blocked merge. The
    // old fail-soft `acquire` left that window open.
    let _lock = match LeaseLock::acquire_or_skip(cwd) {
        Some(l) => l,
        None => return Ok(()),
    };
    if read_merge_conflicts(cwd)?
        .iter()
        .any(|e| e.conflict_id == entry.conflict_id)
    {
        return Ok(());
    }
    artificial_delay("OVERWATCH_TEST_MERGE_CONFLICT_DELAY_MS");
    let json = serde_json::to_string(entry)?;
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?
        .write_all(format!("{}\n", json).as_bytes())?;
    Ok(())
}

/// Read all blocked-merge entries (fail-soft, corrupt lines skipped).
pub fn read_merge_conflicts(cwd: &Path) -> Result<Vec<MergeConflictEntry>> {
    let path = merge_conflicts_path(cwd)?;
    match std::fs::read_to_string(&path) {
        Ok(txt) => {
            let mut entries = Vec::new();
            for line in txt.lines() {
                if !line.is_empty() {
                    if let Ok(e) = serde_json::from_str::<MergeConflictEntry>(line) {
                        entries.push(e);
                    }
                }
            }
            Ok(entries)
        }
        Err(_) => Ok(Vec::new()),
    }
}

/// Append one resolution to `merge_conflict_resolutions.jsonl`.
///
/// Idempotent per `conflict_id`: two concurrent resolvers (e.g. a human via
/// `resolve-merge-conflict` and condukt's policy) that both resolve the SAME
/// conflict must not double-row it. Same check-then-append TOCTOU guard as
/// [`append_disposition`]: hold the store `LeaseLock`, re-check for an existing
/// resolution of this `conflict_id` INSIDE the lock, skip if present. Fail-soft:
/// `LeaseLock` degrades to unlocked on timeout and never panics.
///
/// Returns an [`AppendOutcome`] (like [`append_disposition`]) so the
/// `resolve-merge-conflict` CLI can report a contended HARD-SKIP truthfully
/// (`resolved:false`) instead of a phantom `resolved:true` while nothing was
/// persisted.
pub fn append_merge_conflict_resolution(
    cwd: &Path,
    resolution: &MergeConflictResolution,
) -> Result<AppendOutcome> {
    let path = merge_conflict_resolutions_path(cwd)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // HARD-SKIP on contention: skip the append rather than proceed to an
    // unlocked check-then-append that could double-row the resolution. The old
    // fail-soft `acquire` left that window open.
    let _lock = match LeaseLock::acquire_or_skip(cwd) {
        Some(l) => l,
        None => return Ok(AppendOutcome::SkippedContended),
    };
    if read_merge_conflict_resolutions(cwd)?
        .iter()
        .any(|r| r.conflict_id == resolution.conflict_id)
    {
        // Already present (idempotent dedup): the resolution IS persisted.
        return Ok(AppendOutcome::Recorded);
    }
    artificial_delay("OVERWATCH_TEST_MERGE_RESOLUTION_DELAY_MS");
    let json = serde_json::to_string(resolution)?;
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?
        .write_all(format!("{}\n", json).as_bytes())?;
    Ok(AppendOutcome::Recorded)
}

/// Read all merge-conflict resolutions (fail-soft, corrupt lines skipped).
pub fn read_merge_conflict_resolutions(cwd: &Path) -> Result<Vec<MergeConflictResolution>> {
    let path = merge_conflict_resolutions_path(cwd)?;
    match std::fs::read_to_string(&path) {
        Ok(txt) => {
            let mut resolutions = Vec::new();
            for line in txt.lines() {
                if !line.is_empty() {
                    if let Ok(r) = serde_json::from_str::<MergeConflictResolution>(line) {
                        resolutions.push(r);
                    }
                }
            }
            Ok(resolutions)
        }
        Err(_) => Ok(Vec::new()),
    }
}

/// The OPEN blocked-merge set: entries with no resolution (fail-soft read of
/// both streams, joined by `conflict_id`). This is what `review-queue`
/// surfaces as `[merge-conflict]` rows.
pub fn open_merge_conflicts(cwd: &Path) -> Result<Vec<MergeConflictEntry>> {
    let entries = read_merge_conflicts(cwd)?;
    let resolutions = read_merge_conflict_resolutions(cwd)?;
    Ok(crate::merge_conflict::open_entries(&entries, &resolutions))
}

/// Look up the resolution for a `conflict_id`, if any (for the condukt
/// reconciliation driver that reads a human/policy decision back).
pub fn find_merge_conflict_resolution(
    cwd: &Path,
    conflict_id: &str,
) -> Result<Option<MergeConflictResolution>> {
    Ok(read_merge_conflict_resolutions(cwd)?
        .into_iter()
        .find(|r| r.conflict_id == conflict_id))
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

/// Guards any test (in this module or elsewhere in the crate) that sandboxes
/// the process-global `$HOME` env var, since `storage_root` resolves under
/// the REAL `$HOME` (via `harness_core::config::home`) regardless of the
/// caller-supplied `cwd`. A SINGLE crate-wide lock is required: two tests in
/// DIFFERENT modules each guarded by their own separate `Mutex` would still
/// race each other's `$HOME` mutation (env vars are process-global, and
/// `cargo test` runs threads within one process) — that's exactly the bug
/// this static fixes by being the one lock every such test shares.
#[cfg(test)]
pub(crate) static HOME_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    fn viol_event(sig: &str, ts: i64) -> ViolationEvent {
        ViolationEvent {
            source: crate::violation::ViolationSource::Blastguard,
            signature: sig.to_string(),
            task_key: "task-key".to_string(),
            session_id: "session".to_string(),
            ts,
            detail: None,
        }
    }

    fn scan_test_home() -> (std::path::PathBuf, Option<std::ffi::OsString>) {
        // Process-unique dir (pid + nanos + monotonic seq — NOT seconds), so the
        // three HOME-locked scan tests that run back-to-back within one wall
        // clock second never share a dir and poison each other's append-only
        // violations.jsonl. Mirrors `unique_tmp`'s uniqueness recipe.
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let prev_home = std::env::var_os("HOME");
        let dir = std::env::temp_dir().join(format!(
            "overwatch-scan-violations-{}-{}-{}",
            std::process::id(),
            now_unix_nanos(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("HOME", &dir);
        (dir, prev_home)
    }

    fn restore_home(prev_home: Option<std::ffi::OsString>) {
        match prev_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
    }

    // Absent registry (nothing ever recorded) is a LEGITIMATELY-empty history —
    // the normal first-deploy case — and must scan as `Absent`, NOT
    // `Undetermined`, so the canary gate proceeds instead of rolling back every
    // fresh rollout.
    #[test]
    fn scan_violations_absent_when_file_missing() {
        let _guard = HOME_ENV_LOCK.lock().unwrap();
        let (dir, prev_home) = scan_test_home();
        assert!(matches!(scan_violations(&dir), ViolationScan::Absent));
        restore_home(prev_home);
    }

    // A clean, fully-parseable registry scans as `Events` with every line — the
    // authoritative count. Distinct from `Absent` (an empty file present but
    // holding no events still yields `Events(empty)`, never `Undetermined`).
    #[test]
    fn scan_violations_events_when_all_lines_parse() {
        let _guard = HOME_ENV_LOCK.lock().unwrap();
        let (dir, prev_home) = scan_test_home();
        append_violation(&dir, &viol_event("blastguard:rm-rf", 1_700_000_000)).unwrap();
        append_violation(&dir, &viol_event("blastguard:rm-rf", 1_700_000_001)).unwrap();
        let scan = scan_violations(&dir);
        let len = match &scan {
            ViolationScan::Events(v) => v.len(),
            _ => usize::MAX,
        };
        assert_eq!(len, 2, "expected Events(2), got {scan:?}");
        restore_home(prev_home);
    }

    // The core fail-CLOSED contract: a present registry with even ONE
    // unparseable (schema-drifted / corrupt) line scans as `Undetermined`, NOT
    // a silently-under-counted `Events`. A dropped line could be a real
    // violation the fleet gate must not go blind to.
    #[test]
    fn scan_violations_undetermined_on_unparseable_line() {
        let _guard = HOME_ENV_LOCK.lock().unwrap();
        let (dir, prev_home) = scan_test_home();
        // One valid event, then a corrupt (non-JSON / schema-drifted) line.
        append_violation(&dir, &viol_event("blastguard:rm-rf", 1_700_000_000)).unwrap();
        let path = violations_path(&dir).unwrap();
        {
            use std::io::Write as _;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            writeln!(f, "{{not-valid-json-schema-drift").unwrap();
        }
        assert!(
            matches!(scan_violations(&dir), ViolationScan::Undetermined),
            "a corrupt line must make the whole scan Undetermined (fail closed), \
             not a silently under-counted Events"
        );
        restore_home(prev_home);
    }

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

    // Two concurrent degraded writers saving the SAME lease registry to one path
    // must never leave a half-written/empty leases.json behind. `save_leases` now
    // uses `unique_tmp`, so each writer renames its OWN fully-written temp
    // atomically and a concurrent reader always sees a complete registry. Under
    // the old fixed `json.tmp` name both writers share one temp and one can rename
    // a partially written file (corrupt leases → `load_leases` fails soft to an
    // EMPTY registry → every lease vanishes → mass double-claim). Large registry
    // so a single write is not atomic at the OS level (widens the corrupt window).
    #[test]
    fn concurrent_save_leases_never_publishes_corrupt_registry() {
        let _guard = HOME_ENV_LOCK.lock().unwrap();
        let prev_home = std::env::var_os("HOME");
        let dir = std::env::temp_dir().join(format!(
            "overwatch-store-test-concurrent-leases-{}-{}",
            std::process::id(),
            now()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("HOME", &dir);

        let mut leases = LeaseRegistry::new();
        for i in 0..400 {
            let key = format!("task-key-{i:04}-with-a-longish-identifier");
            leases.insert(
                key.clone(),
                Lease {
                    key,
                    title: format!("task number {i} with a reasonably long title"),
                    session_id: format!("session-{i:04}"),
                    run_id: format!("run-{i:04}"),
                    claimed_at: 1_700_000_000 + i as i64,
                    heartbeat_at: 1_700_000_000 + i as i64,
                    scope: vec![format!("crates/pkg/src/file_{i:04}.rs")],
                    done_criteria: Some(format!("criterion for task {i}")),
                },
            );
        }
        let expected = leases.len();

        const THREADS: usize = 8;
        const ITERS: usize = 30;
        std::thread::scope(|scope| {
            for _ in 0..THREADS {
                let dir = dir.clone();
                let leases = leases.clone();
                scope.spawn(move || {
                    for _ in 0..ITERS {
                        save_leases(&dir, &leases).unwrap();
                        // A concurrent reader must never observe a corrupt/empty
                        // registry (fail-soft would swallow corruption as empty).
                        let loaded = load_leases(&dir).unwrap();
                        assert_eq!(
                            loaded.len(),
                            expected,
                            "a concurrent reader observed a corrupt/empty leases.json"
                        );
                    }
                });
            }
        });

        let final_reg = load_leases(&dir).unwrap();
        assert_eq!(final_reg.len(), expected);

        match prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    // A wedged holder of the store lock must make the REAL production
    // check-then-append path HARD-SKIP (append NOTHING) rather than degrade to
    // an unlocked check-then-append that could double-row the disposition. RED
    // with the old fail-soft `acquire`: it hands back an unlocked guard, the
    // append proceeds, and dispositions.jsonl gains a line. GREEN with
    // `acquire_or_skip_with_deadline`.
    #[test]
    fn contended_store_lock_makes_append_disposition_skip_not_double_write() {
        use crate::disposition::{Disposition, DispositionVerdict};
        use crate::lock::LeaseLock;

        let _guard = HOME_ENV_LOCK.lock().unwrap();
        let prev_home = std::env::var_os("HOME");
        let dir = std::env::temp_dir().join(format!(
            "overwatch-store-test-append-skip-{}-{}",
            std::process::id(),
            now()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("HOME", &dir);

        // Wedge a live holder of the store lock for the whole critical section.
        let held = LeaseLock::acquire(&dir);
        assert!(
            held.held(),
            "precondition: wedge must genuinely hold the store lock"
        );

        let disp = Disposition {
            finding_id: "finding-1".to_string(),
            verdict: DispositionVerdict::Confirmed,
            reviewer: "tester".to_string(),
            resolved_ts: now(),
        };

        // The real production path must SKIP the append under contention, driven
        // with a short deadline so the test stays fast.
        let outcome =
            append_disposition_with_deadline(&dir, &disp, std::time::Duration::from_millis(120))
                .unwrap();

        // The skip must be OBSERVABLE (not a phantom success): the CLI relies on
        // this to print `recorded:false` instead of `recorded:true` while the
        // ledger is untouched. RED with a bare `Ok(())`/`Ok(Recorded)` return.
        assert_eq!(
            outcome,
            AppendOutcome::SkippedContended,
            "a contended append must report SkippedContended, not a phantom Recorded"
        );

        // Nothing was appended (no unlocked double-row).
        let dispositions = read_dispositions(&dir).unwrap();
        assert!(
            dispositions.is_empty(),
            "a contended append_disposition must not write any line, got {dispositions:?}"
        );

        drop(held);
        match prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        std::fs::remove_dir_all(&dir).ok();
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
                scope: Vec::new(),
                done_criteria: None,
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
    fn lease_without_new_fields_deserializes_with_defaults() {
        // A pre-existing leases.json entry (no scope / done_criteria) must still
        // load — `#[serde(default)]` fills the PDO anchor fields.
        let legacy = r#"{
            "key": "k",
            "title": "t",
            "session_id": "s",
            "run_id": "r",
            "claimed_at": 1,
            "heartbeat_at": 2
        }"#;
        let lease: Lease = serde_json::from_str(legacy).expect("legacy lease parses");
        assert!(lease.scope.is_empty());
        assert_eq!(lease.done_criteria, None);
    }

    #[test]
    fn lease_with_anchor_fields_roundtrips() {
        let lease = Lease {
            key: "k".to_string(),
            title: "t".to_string(),
            session_id: "s".to_string(),
            run_id: "r".to_string(),
            claimed_at: 1,
            heartbeat_at: 2,
            scope: vec!["crates/overwatch/src/**".to_string()],
            done_criteria: Some("all tests green".to_string()),
        };
        let json = serde_json::to_string(&lease).unwrap();
        let back: Lease = serde_json::from_str(&json).unwrap();
        assert_eq!(back.scope, lease.scope);
        assert_eq!(back.done_criteria, lease.done_criteria);
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
            scope: Vec::new(),
            done_criteria: None,
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
                scope: Vec::new(),
                done_criteria: None,
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
                scope: Vec::new(),
                done_criteria: None,
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

    fn finding(id: &str, ts: i64) -> ReviewFinding {
        ReviewFinding::new(
            id.to_string(),
            "reviewgate".to_string(),
            None,
            "s".to_string(),
            None,
            None,
            ts,
        )
    }

    #[test]
    fn partition_findings_resolved_goes_to_archived_open_stays() {
        let findings = vec![finding("a", 1), finding("b", 2), finding("c", 3)];
        let resolved: BTreeSet<String> = ["b".to_string()].into_iter().collect();
        let (open, archived) = partition_findings(&findings, &resolved);
        assert_eq!(open, vec![finding("a", 1), finding("c", 3)]);
        assert_eq!(archived, vec![finding("b", 2)]);
    }

    #[test]
    fn partition_findings_preserves_multiplicity_of_open_id() {
        // A still-open id re-recorded across rounds must keep ALL of its
        // occurrences in `open` (occurrence count preserved).
        let findings = vec![finding("a", 1), finding("a", 2), finding("b", 3)];
        let resolved: BTreeSet<String> = BTreeSet::new();
        let (open, archived) = partition_findings(&findings, &resolved);
        assert_eq!(
            open,
            vec![finding("a", 1), finding("a", 2), finding("b", 3)]
        );
        assert!(archived.is_empty());
    }

    #[test]
    fn partition_findings_preserves_append_order_in_both_partitions() {
        let findings = vec![
            finding("z", 1),
            finding("a", 2),
            finding("y", 3),
            finding("b", 4),
        ];
        let resolved: BTreeSet<String> = ["z".to_string(), "y".to_string()].into_iter().collect();
        let (open, archived) = partition_findings(&findings, &resolved);
        // Survivors keep their ORIGINAL relative (append) order, not the
        // BTreeSet's lexicographic order.
        assert_eq!(open, vec![finding("a", 2), finding("b", 4)]);
        assert_eq!(archived, vec![finding("z", 1), finding("y", 3)]);
    }

    #[test]
    fn partition_findings_resolved_id_absent_from_findings_is_noop() {
        let findings = vec![finding("a", 1)];
        let resolved: BTreeSet<String> = ["nonexistent".to_string()].into_iter().collect();
        let (open, archived) = partition_findings(&findings, &resolved);
        assert_eq!(open, vec![finding("a", 1)]);
        assert!(archived.is_empty());
    }

    #[test]
    fn partition_findings_empty_inputs() {
        let (open, archived) = partition_findings(&[], &BTreeSet::new());
        assert!(open.is_empty());
        assert!(archived.is_empty());
    }

    #[test]
    fn partition_findings_is_deterministic() {
        let findings = vec![finding("a", 1), finding("b", 2), finding("c", 3)];
        let resolved: BTreeSet<String> = ["a".to_string(), "c".to_string()].into_iter().collect();
        let run1 = partition_findings(&findings, &resolved);
        let run2 = partition_findings(&findings, &resolved);
        assert_eq!(run1, run2);
    }

    #[test]
    fn read_review_findings_all_concatenates_archive_after_hot() {
        let _guard = HOME_ENV_LOCK.lock().unwrap();
        let prev_home = std::env::var_os("HOME");
        let dir = std::env::temp_dir().join(format!(
            "overwatch-store-test-all-{}-{}",
            std::process::id(),
            now()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("HOME", &dir);

        let hot = review_findings_path(&dir).unwrap();
        std::fs::create_dir_all(hot.parent().unwrap()).unwrap();
        std::fs::write(
            &hot,
            format!("{}\n", serde_json::to_string(&finding("a", 1)).unwrap()),
        )
        .unwrap();

        // Missing archive => hot only.
        let hot_only = read_review_findings_all(&dir).unwrap();
        assert_eq!(hot_only, vec![finding("a", 1)]);

        // Present archive (with a trailing corrupt line) => hot then archive,
        // corrupt line skipped.
        let archive = review_findings_archive_path(&dir).unwrap();
        std::fs::write(
            &archive,
            format!(
                "{}\nnot valid json\n",
                serde_json::to_string(&finding("b", 2)).unwrap()
            ),
        )
        .unwrap();
        let combined = read_review_findings_all(&dir).unwrap();
        assert_eq!(combined, vec![finding("a", 1), finding("b", 2)]);

        match prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn bridged_entries_ledger_roundtrips_and_skips_corrupt_lines() {
        let _guard = HOME_ENV_LOCK.lock().unwrap();
        let prev_home = std::env::var_os("HOME");
        let dir = std::env::temp_dir().join(format!(
            "overwatch-store-test-entries-{}-{}",
            std::process::id(),
            now()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("HOME", &dir);

        // Missing ledger => empty (fail-soft).
        assert!(read_bridged_entries(&dir).unwrap().is_empty());

        // Append two keys, then a hand-written corrupt line that must be skipped.
        append_bridged_entry(&dir, "rollback:overwatch").unwrap();
        append_bridged_entry(&dir, "systemic:blastguard:rm-rf").unwrap();
        let path = bridged_entries_path(&dir).unwrap();
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"not valid json\n")
            .unwrap();

        let keys = read_bridged_entries(&dir).unwrap();
        assert_eq!(
            keys,
            vec!["rollback:overwatch", "systemic:blastguard:rm-rf"]
        );

        // The entries ledger is a SEPARATE file from bridged_findings.jsonl, so
        // it must not disturb the finding-resolution source.
        assert!(read_bridged_findings(&dir).unwrap().is_empty());

        match prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    // AXIS 2: a finding appended concurrently with `compact_review_findings`'s
    // read->rewrite must NOT be dropped. Compaction now holds the store
    // LeaseLock across its whole read->rewrite, so a lock-respecting concurrent
    // appender serializes against it instead of having its append clobbered by
    // the stale-snapshot hot rewrite.
    //
    // RED (before the fix): remove the `LeaseLock::acquire` in
    // `compact_review_findings` and this fails — `f3` is dropped by the hot
    // rewrite. GREEN: with the lock, `f3` survives in the hot store.
    #[test]
    fn concurrent_append_survives_compact_review_findings() {
        use crate::lock::LeaseLock;
        let _guard = HOME_ENV_LOCK.lock().unwrap();
        let prev_home = std::env::var_os("HOME");
        let dir = std::env::temp_dir().join(format!(
            "overwatch-compact-race-{}-{}",
            std::process::id(),
            now()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("HOME", &dir);
        std::env::set_var("OVERWATCH_TEST_COMPACT_DELAY_MS", "300");

        // f1 stays open; f2 is resolved (bridged) so compaction archives it.
        append_review_finding(&dir, &finding("f1", 1)).unwrap();
        append_review_finding(&dir, &finding("f2", 2)).unwrap();
        append_bridged_finding(&dir, "f2").unwrap();

        // Thread A: compact (locked read->rewrite, delayed).
        let dir_a = dir.clone();
        let a = std::thread::spawn(move || {
            compact_review_findings(&dir_a).expect("compact ok");
        });

        // Give A a moment to acquire the lock and enter its delay, then run a
        // lock-respecting appender that records a NEW open finding f3.
        std::thread::sleep(std::time::Duration::from_millis(50));
        {
            let _l = LeaseLock::acquire(&dir);
            append_review_finding(&dir, &finding("f3", 3)).unwrap();
        }

        a.join().unwrap();

        let hot = read_review_findings(&dir).unwrap();
        assert!(
            hot.iter().any(|f| f.finding_id == "f3"),
            "concurrently appended f3 must not be dropped by compaction; hot={:?}",
            hot.iter()
                .map(|f| f.finding_id.as_str())
                .collect::<Vec<_>>()
        );
        // Sanity: compaction ran (f1 stays open, resolved f2 was archived out).
        assert!(hot.iter().any(|f| f.finding_id == "f1"));
        assert!(
            !hot.iter().any(|f| f.finding_id == "f2"),
            "resolved f2 should have been archived out of the hot store"
        );

        std::env::remove_var("OVERWATCH_TEST_COMPACT_DELAY_MS");
        match prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    // AXIS 3: two concurrent `to-backlog` runs appending the SAME bridged
    // finding-id must NOT double-add. `append_bridged_finding` now holds the
    // store LeaseLock AND re-checks membership inside it, so exactly one record
    // survives.
    //
    // RED (before the fix): the unlocked, non-idempotent append always writes,
    // so both runs pass the (absent) check inside the widened window and TWO
    // "x" lines land. GREEN: lock + in-lock recheck => exactly one.
    #[test]
    fn concurrent_bridged_finding_append_does_not_double_add() {
        let _guard = HOME_ENV_LOCK.lock().unwrap();
        let prev_home = std::env::var_os("HOME");
        let dir = std::env::temp_dir().join(format!(
            "overwatch-bridged-race-{}-{}",
            std::process::id(),
            now()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("HOME", &dir);
        std::env::set_var("OVERWATCH_TEST_BRIDGE_DELAY_MS", "200");

        // Two concurrent runs append the SAME finding-id "x".
        let d1 = dir.clone();
        let t1 = std::thread::spawn(move || append_bridged_finding(&d1, "x").unwrap());
        let d2 = dir.clone();
        let t2 = std::thread::spawn(move || append_bridged_finding(&d2, "x").unwrap());
        t1.join().unwrap();
        t2.join().unwrap();

        let ids = read_bridged_findings(&dir).unwrap();
        let count = ids.iter().filter(|id| id.as_str() == "x").count();
        assert_eq!(
            count, 1,
            "bridged finding-id x must appear exactly once; got {ids:?}"
        );

        std::env::remove_var("OVERWATCH_TEST_BRIDGE_DELAY_MS");
        match prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    // Two concurrent disposition appends of the SAME finding-id must NOT
    // double-row it. `append_disposition` now holds the store LeaseLock AND
    // re-checks for an existing disposition with that finding_id INSIDE the
    // lock, so exactly one row survives. Because the dedup lives at the shared
    // write site, BOTH callers are covered: this simulates the cross-caller
    // race (a manual `record-disposition` racing a `reconcile-fixed` append of
    // the same finding_id) — either ordering yields a single row.
    //
    // RED (before the fix): the unlocked, non-idempotent append always writes,
    // so both writers pass the (absent) check inside the widened window and TWO
    // rows for the same finding_id land. GREEN: lock + in-lock recheck => one.
    #[test]
    fn concurrent_disposition_append_dedupes_on_finding_id() {
        use crate::disposition::{Disposition, DispositionVerdict};

        let _guard = HOME_ENV_LOCK.lock().unwrap();
        let prev_home = std::env::var_os("HOME");
        let dir = std::env::temp_dir().join(format!(
            "overwatch-disp-race-{}-{}",
            std::process::id(),
            now()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("HOME", &dir);
        std::env::set_var("OVERWATCH_TEST_DISPOSITION_DELAY_MS", "200");

        // Writer A: a "reconcile-fixed"-style append. Writer B: a manual
        // "record-disposition"-style append. Same finding_id "f1", distinct
        // reviewers to mirror the two callers racing at the shared write site.
        let d1 = dir.clone();
        let t1 = std::thread::spawn(move || {
            let disp = Disposition::new(
                "f1".to_string(),
                DispositionVerdict::Confirmed,
                "reconcile-fixed".to_string(),
                now(),
            );
            append_disposition(&d1, &disp).unwrap();
        });
        let d2 = dir.clone();
        let t2 = std::thread::spawn(move || {
            let disp = Disposition::new(
                "f1".to_string(),
                DispositionVerdict::Dismissed,
                "human".to_string(),
                now(),
            );
            append_disposition(&d2, &disp).unwrap();
        });
        t1.join().unwrap();
        t2.join().unwrap();

        let dispositions = read_dispositions(&dir).unwrap();
        let count = dispositions.iter().filter(|d| d.finding_id == "f1").count();
        assert_eq!(
            count, 1,
            "disposition finding_id f1 must appear exactly once; got {dispositions:?}"
        );

        std::env::remove_var("OVERWATCH_TEST_DISPOSITION_DELAY_MS");
        match prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    // ── Mid-flight runtime-conflict store (design 625aa170 A) ────────────────

    use crate::changeset::ActualChangeset;

    fn changeset(task_key: &str, files: &[&str], ts: i64) -> ActualChangeset {
        ActualChangeset::new(
            task_key.to_string(),
            task_key.split('/').next().unwrap_or("run").to_string(),
            "sess".to_string(),
            format!("condukt/{task_key}"),
            "base".to_string(),
            "head".to_string(),
            &files.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            ts,
        )
    }

    /// Round-trip: recording a first changeset yields no overlap; recording a
    /// second one that shares an (undeclared) file returns exactly one event
    /// naming that file, the registry holds both, and the event is persisted to
    /// `runtime_conflicts.jsonl`. Store isolation via a per-test sandboxed HOME.
    #[test]
    fn record_changeset_and_detect_round_trips_overlap() {
        let _guard = HOME_ENV_LOCK.lock().unwrap();
        let prev_home = std::env::var_os("HOME");
        let dir = std::env::temp_dir().join(format!(
            "overwatch-changeset-rt-{}-{}",
            std::process::id(),
            now()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("HOME", &dir);

        let first =
            record_changeset_and_detect(&dir, &changeset("runA/t1", &["shared.rs", "a.rs"], 100))
                .unwrap();
        assert!(first.is_empty(), "no peers yet -> no overlap");

        let second =
            record_changeset_and_detect(&dir, &changeset("runA/t2", &["shared.rs", "b.rs"], 110))
                .unwrap();
        assert_eq!(second.len(), 1, "one overlapping peer");
        assert_eq!(second[0].overlapping_files, vec!["shared.rs".to_string()]);
        assert_eq!(second[0].task_key_a, "runA/t2");
        assert_eq!(second[0].task_key_b, "runA/t1");

        let reg = load_active_changesets(&dir).unwrap();
        assert_eq!(reg.len(), 2, "both changesets upserted");

        let persisted = read_runtime_conflicts(&dir).unwrap();
        assert_eq!(persisted.len(), 1, "the overlap event was appended");
        assert_eq!(
            persisted[0].overlapping_files,
            vec!["shared.rs".to_string()]
        );

        // Cleanup / merged: mark t1 merged -> a later t3 sharing the file no
        // longer overlaps it (merged excluded).
        mark_changeset_merged(&dir, "runA/t1").unwrap();
        let third =
            record_changeset_and_detect(&dir, &changeset("runA/t3", &["shared.rs"], 120)).unwrap();
        assert_eq!(third.len(), 1, "still overlaps live t2, but NOT merged t1");
        assert!(third.iter().all(|e| e.task_key_b == "runA/t2"));

        match prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Finding #1 production wiring: `mark_branch_merged` (branch-keyed, what the
    /// merge path can actually call — it only knows the branch, not the
    /// `task_key`) takes a LANDED branch's changeset out of the detection set, so
    /// a later sequential task that touches a common file is NOT spuriously
    /// flagged. Also asserts `prune_stale_changesets` compacts merged entries.
    #[test]
    fn mark_branch_merged_excludes_a_landed_branch_from_detection() {
        let _guard = HOME_ENV_LOCK.lock().unwrap();
        let prev_home = std::env::var_os("HOME");
        let dir = std::env::temp_dir().join(format!(
            "overwatch-branch-merged-{}-{}",
            std::process::id(),
            now()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("HOME", &dir);

        // t1 finishes first (no peers -> no overlap). Its branch is condukt/runA/t1.
        let first =
            record_changeset_and_detect(&dir, &changeset("runA/t1", &["shared.rs"], 100)).unwrap();
        assert!(first.is_empty(), "no peers yet");

        // t1 LANDS: mark it merged by BRANCH (the only key the merge path has).
        let marked = mark_branch_merged(&dir, "condukt/runA/t1").unwrap();
        assert_eq!(marked, 1, "the landed branch's changeset is marked merged");
        // Idempotent: a second call marks nothing (already merged).
        assert_eq!(mark_branch_merged(&dir, "condukt/runA/t1").unwrap(), 0);

        // t2 finishes later touching the SAME file — must NOT overlap the landed t1.
        let second =
            record_changeset_and_detect(&dir, &changeset("runA/t2", &["shared.rs"], 110)).unwrap();
        assert!(
            second.is_empty(),
            "a landed (merged) peer must be excluded from overlap detection; got {second:?}"
        );

        // Housekeeping: pruning drops the merged t1 (bounded registry, finding #4).
        let removed = prune_stale_changesets(&dir, 120).unwrap();
        assert_eq!(removed, 1, "the merged changeset is pruned");
        let reg = load_active_changesets(&dir).unwrap();
        assert!(!reg.contains_key("runA/t1"), "merged entry pruned");
        assert!(reg.contains_key("runA/t2"), "live entry retained");

        match prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Two tasks recording DISTINCT changesets concurrently must both survive:
    /// without the `LeaseLock` serialization, one thread's load->save would
    /// clobber the other's upsert (lost update), leaving the registry with one
    /// entry instead of two — and silently missing a real cross-task overlap.
    /// The `OVERWATCH_TEST_CHANGESET_DELAY_MS` widener forces the interleave.
    #[test]
    fn concurrent_record_changeset_never_loses_an_upsert() {
        let _guard = HOME_ENV_LOCK.lock().unwrap();
        let prev_home = std::env::var_os("HOME");
        let dir = std::env::temp_dir().join(format!(
            "overwatch-changeset-conc-{}-{}",
            std::process::id(),
            now()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("HOME", &dir);
        std::env::set_var("OVERWATCH_TEST_CHANGESET_DELAY_MS", "200");

        let d1 = dir.clone();
        let t1 = std::thread::spawn(move || {
            record_changeset_and_detect(&d1, &changeset("runX/ta", &["a.rs"], now())).unwrap();
        });
        let d2 = dir.clone();
        let t2 = std::thread::spawn(move || {
            record_changeset_and_detect(&d2, &changeset("runX/tb", &["b.rs"], now())).unwrap();
        });
        t1.join().unwrap();
        t2.join().unwrap();

        let reg = load_active_changesets(&dir).unwrap();
        assert_eq!(
            reg.len(),
            2,
            "both concurrent upserts must survive (LeaseLock serialization); got {reg:?}"
        );

        std::env::remove_var("OVERWATCH_TEST_CHANGESET_DELAY_MS");
        match prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }
}
