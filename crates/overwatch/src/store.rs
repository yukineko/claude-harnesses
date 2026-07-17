/// Event store and lease storage backend.
use crate::audit_round::AuditRound;
use crate::disposition::Disposition;
use crate::event::LifecycleEvent;
use crate::lock::LeaseLock;
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
    let tmp = path.with_extension("json.tmp");
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
    // (the shared append site) covers ALL callers. Fail-soft: LeaseLock degrades
    // to unlocked on timeout and never panics.
    let _lock = LeaseLock::acquire(cwd);
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
    // Fail-soft: LeaseLock degrades to unlocked on timeout and never panics.
    let _lock = LeaseLock::acquire(cwd);
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

/// Append a human disposition to dispositions.jsonl (one JSON line each).
pub fn append_disposition(cwd: &Path, disposition: &Disposition) -> Result<()> {
    let path = dispositions_path(cwd)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string(disposition)?;
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?
        .write_all(format!("{}\n", json).as_bytes())?;
    Ok(())
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
    // from a stale snapshot taken before the append, clobbering it. Fail-soft:
    // LeaseLock degrades to unlocked on timeout and never panics.
    let _lock = LeaseLock::acquire(cwd);

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
}
