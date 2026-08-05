/// Event store and lease storage backend.
use crate::audit_round::{self, AuditRound};
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
use harness_core::verdict::Determination;
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

/// Load the lease registry, distinguishing a genuinely-absent leases.json
/// (legitimate cold-start empty registry) from one that exists but could not
/// be trusted (unreadable, or present but corrupt/unparseable). Reads via
/// [`harness_core::boundary::read_to_string`] (mirrors [`scan_review_findings`])
/// so the absent/opaque distinction is drawn by the shared boundary type
/// rather than re-derived locally.
///
/// Callers do `load_leases(cwd)?` then mutate the registry then
/// `save_leases(cwd, ..)` (an atomic temp+rename read-modify-write). If an
/// unreadable or corrupt leases.json were folded into an empty registry (as
/// the old fail-soft behavior did), that read-modify-write would silently
/// clobber every other session's lease with an empty registry on the
/// subsequent write (mass double-claim). So only a genuinely-absent file
/// yields `Ok(LeaseRegistry::default())`; an unreadable or corrupt file
/// yields `Err`, aborting the mutator before it can write anything back.
pub fn load_leases(cwd: &Path) -> Result<LeaseRegistry> {
    let path = leases_path(cwd)?;
    match harness_core::boundary::read_to_string(&path) {
        harness_core::verdict::Determination::Known(None) => Ok(LeaseRegistry::default()),
        harness_core::verdict::Determination::Known(Some(txt)) => serde_json::from_str::<
            LeaseRegistry,
        >(&txt)
        .map_err(|e| anyhow::anyhow!("leases.json present but corrupt at {}: {e}", path.display())),
        harness_core::verdict::Determination::Undetermined(reason) => {
            anyhow::bail!(
                "leases.json could not be read at {}: {reason}",
                path.display()
            )
        }
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

// ── Tri-state ledger reads ───────────────────────────────────────────────────
//
// Every append-only ledger in this module used to be read the same two-valued
// way: `Err(_) => Ok(Vec::new())` for the file, `if let Ok(x) = from_str(line)`
// for each line. Both halves map "I could not read this" onto the same bytes as
// "there is nothing here", and every consumer downstream reads the second.
//
// WHICH tri-state type: the SHARED `harness_core::verdict::Determination<T>`
// (already used in this file by `load_leases` and `read_audit_rounds`), not a
// new enum per reader. `Determination` says exactly the two things these
// ledgers need — `Known(rows)` for an observation, `Undetermined(why)` for an
// opacity that carries its reason — and an absent append-only ledger is
// honestly `Known(vec![])`: nothing was ever appended, which is a real
// measurement of zero.
//
// The two bespoke enums here ([`ViolationScan`], [`ReviewFindingScan`]) keep
// their separate `Absent` variant because their call sites BRANCH on it: the
// canary gate must not roll back a first deploy, so "never written" and "read
// clean, held nothing" are different answers THERE. No such call site exists
// for the ledgers below, so they do not get a third variant that nobody reads.

/// Decode every non-blank line of a JSONL ledger.
///
/// Returns the records that DID decode, together with the reason this pass
/// cannot be called complete (the first undecodable line, with its 1-based line
/// number). Both halves come from one place on purpose: the tri-state `scan_*`
/// readers and the legacy best-effort `read_*` readers share this decode and
/// differ only in what they do with a `Some(reason)`, so the two can never
/// drift into disagreeing about what the file holds.
fn decode_jsonl_lines<T: serde::de::DeserializeOwned>(
    txt: &str,
    path: &Path,
    ledger: &str,
) -> (Vec<T>, Option<String>) {
    let mut records = Vec::new();
    let mut undecodable: Option<String> = None;
    for (i, line) in txt.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<T>(line) {
            Ok(r) => records.push(r),
            Err(e) => {
                if undecodable.is_none() {
                    undecodable = Some(format!(
                        "{ledger} at {} holds an undecodable line (line {}): {e}. The records \
                         that did decode are a PARTIAL view and would be indistinguishable from \
                         a complete one.",
                        path.display(),
                        i + 1
                    ));
                }
            }
        }
    }
    (records, undecodable)
}

/// Tri-state read of an append-only JSONL ledger — the sanctioned reader when
/// the answer feeds a decision.
///
/// * absent file → `Known(vec![])`. Nothing was ever appended; that is a real
///   observation of zero, not a failure (see the module note above).
/// * present and fully decodable → `Known(rows)`.
/// * present but unreadable (permissions, non-UTF-8, I/O) → `Undetermined`,
///   forwarded from [`harness_core::boundary::read_to_string`].
/// * present but holding even ONE undecodable line → `Undetermined`. A dropped
///   line is not "one fewer record": it may be the exact record the caller was
///   about to conclude did not exist.
fn scan_jsonl<T: serde::de::DeserializeOwned>(path: &Path, ledger: &str) -> Determination<Vec<T>> {
    match harness_core::boundary::read_to_string(path) {
        Determination::Known(None) => Determination::known(Vec::new()),
        Determination::Known(Some(txt)) => {
            let (records, undecodable) = decode_jsonl_lines(&txt, path, ledger);
            match undecodable {
                None => Determination::known(records),
                Some(why) => Determination::undetermined(why),
            }
        }
        // Forwarded, deliberately not re-minted: the boundary already recorded
        // this `Undetermined` once, and forwarding must not double-count it.
        Determination::Undetermined(why) => Determination::Undetermined(why),
    }
}

/// Best-effort read of an append-only JSONL ledger: the historical two-valued
/// contract, kept for the consumers that still expect it (t3 migrates them).
///
/// It returns whatever decoded and an empty vec for a file it could not read —
/// which is exactly the collapse `scan_jsonl` exists to avoid. Every public
/// `read_*` wrapper around this says so in its own doc and names the `scan_*`
/// sibling to use instead; nothing that makes a DECISION should call one.
fn read_jsonl_best_effort<T: serde::de::DeserializeOwned>(path: &Path, ledger: &str) -> Vec<T> {
    match harness_core::boundary::read_to_string(path) {
        Determination::Known(Some(txt)) => decode_jsonl_lines(&txt, path, ledger).0,
        Determination::Known(None) | Determination::Undetermined(_) => Vec::new(),
    }
}

/// Read all events from events.jsonl, BEST-EFFORT: an absent, unreadable or
/// partially-undecodable ledger all come back as (or short of) an empty vec, so
/// a caller cannot tell "no events" from "could not read the events".
/// Undetermined-aware consumers must use [`scan_events`]; this two-valued
/// reader is kept only for the existing callers that already treat an
/// unreadable ledger as empty by contract.
pub fn read_events(cwd: &Path) -> Result<Vec<LifecycleEvent>> {
    Ok(read_jsonl_best_effort(&events_path(cwd)?, "events.jsonl"))
}

/// Tri-state read of events.jsonl (see [`scan_jsonl`]): a never-written ledger
/// is `Known(vec![])`, while an unreadable one — or one holding an undecodable
/// line — is `Undetermined(why)` and never a shortened event history.
pub fn scan_events(cwd: &Path) -> Result<Determination<Vec<LifecycleEvent>>> {
    Ok(scan_jsonl(&events_path(cwd)?, "events.jsonl"))
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

/// Three-valued result of reading the violation registry: the ONLY sanctioned
/// way to read it. It keeps "genuinely no violations yet" DISTINCT from "cannot
/// be trusted", so no caller can read a broken store as clean. It fully
/// replaced the retired two-valued reader: there is no fail-open `Result`
/// variant left for a caller to reach for.
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

impl ViolationScan {
    /// Fail-closed extractor for callers that only care about the clean and
    /// absent cases and want an ERROR on the untrustworthy one: `Events` yields
    /// its list, `Absent` yields an empty vec (a store that was never written
    /// genuinely holds zero violations), and `Undetermined` is an `Err`.
    ///
    /// This is the exact, fail-closed stand-in the tests use in place of the
    /// retired two-valued reader, which returned an empty vec for BOTH absent
    /// and unreadable/corrupt — collapsing "nothing recorded" into "could not
    /// read". Here the absent case is preserved but the untrustworthy case can
    /// no longer masquerade as empty; the caller must handle the `Err`
    /// (read-back assertions `.expect()` it, so a corrupt store fails loudly
    /// instead of silently reading as empty). Production gating code should
    /// `match` all three arms directly and decide what `Undetermined` means at
    /// its own call site rather than routing through this helper.
    pub fn events_or_empty(self) -> Result<Vec<ViolationEvent>> {
        match self {
            ViolationScan::Events(events) => Ok(events),
            ViolationScan::Absent => Ok(Vec::new()),
            ViolationScan::Undetermined => {
                anyhow::bail!("violation store is Undetermined — it must not be read as empty")
            }
        }
    }
}

/// Strictly scan the violation registry, distinguishing "absent" (legit empty)
/// from "undetermined" (unreadable / partially-unparseable → untrustworthy)
/// from a clean, fully-parsed event list. This is the ONLY sanctioned reader
/// of the violation registry; each caller decides
/// at its own call site what `Undetermined` means there (block, refuse, exit
/// non-zero, or omit-and-announce) so a broken/corrupt store is never silently
/// read as "zero violations → proceed". A single non-empty line that fails to parse
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

/// Read all canary rollback events from rollbacks.jsonl, BEST-EFFORT (same
/// two-valued contract as [`read_events`], and the same blind spot: an
/// unreadable ledger is indistinguishable from "no rollback ever happened").
/// Use [`scan_rollbacks`] anywhere that answer is acted on.
pub fn read_rollbacks(cwd: &Path) -> Result<Vec<RollbackEvent>> {
    Ok(read_jsonl_best_effort(
        &rollbacks_path(cwd)?,
        "rollbacks.jsonl",
    ))
}

/// Tri-state read of rollbacks.jsonl (see [`scan_jsonl`]). "No rollback has
/// ever been recorded" is a genuine `Known(vec![])`; an unreadable or
/// partially-undecodable ledger is `Undetermined(why)`.
pub fn scan_rollbacks(cwd: &Path) -> Result<Determination<Vec<RollbackEvent>>> {
    Ok(scan_jsonl(&rollbacks_path(cwd)?, "rollbacks.jsonl"))
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
///
/// Kept for consumers that already treat "unreadable" as "empty" by contract
/// (`read_review_findings_all`, `compact`, `bridge`); an undecodable line is
/// likewise skipped rather than surfaced. The review-queue VERDICT path does
/// NOT use this reader — see [`scan_review_findings`], which keeps "never
/// written" distinct from "unreadable/corrupt" so a confirmed finding can never
/// be silently dropped by a permission glitch.
pub fn read_review_findings(cwd: &Path) -> Result<Vec<ReviewFinding>> {
    Ok(read_jsonl_best_effort(
        &review_findings_path(cwd)?,
        "review_findings.jsonl",
    ))
}

/// Three-valued result of reading the AI-review findings stream, mirroring
/// [`ViolationScan`] (see that type's doc for the full rationale): "no
/// producer has ever written this file" (`Absent`) must stay distinguishable
/// from "the file is there but could not be trusted" (`Undetermined`), so a
/// CONFIRMED adversarial-review finding can never be silently collapsed into
/// an empty finding set by a permission glitch or a corrupt line.
#[derive(Debug)]
pub enum ReviewFindingScan {
    /// review_findings.jsonl does not exist: no finding has ever been
    /// recorded. Legitimately empty — the normal case while no producer is
    /// wired, or before the first one runs.
    Absent,
    /// The file exists but could not be trusted: unreadable (I/O / permission)
    /// via [`harness_core::boundary::read_to_string`], or read but held a
    /// non-empty line that failed to parse. A caller MUST NOT read this as
    /// "no findings" — the very finding that failed to come back could be a
    /// real, already-CONFIRMED adversarial review result.
    Undetermined(String),
    /// The file was read cleanly and every non-empty line parsed: the
    /// authoritative, trustworthy finding list (possibly empty if the file
    /// existed but held only blank lines).
    Findings(Vec<ReviewFinding>),
}

/// Strictly scan the AI-review findings stream, distinguishing "absent" (legit
/// empty) from "undetermined" (unreadable / partially-unparseable →
/// untrustworthy) from a clean, fully-parsed finding list. This is the
/// sanctioned reader for the review-queue VERDICT path; other established
/// consumers of `review_findings.jsonl` (`read_review_findings_all`,
/// `compact`, `bridge`) keep using [`read_review_findings`] and are out of
/// scope here. Reads via [`harness_core::boundary::read_to_string`] so the
/// absent/opaque distinction is drawn by the shared boundary type, not
/// re-derived locally.
pub fn scan_review_findings(cwd: &Path) -> ReviewFindingScan {
    let path = match review_findings_path(cwd) {
        // Cannot even resolve the storage path (e.g. no HOME): undetermined,
        // not empty — we cannot claim "no findings".
        Err(e) => {
            return ReviewFindingScan::Undetermined(format!(
                "cannot resolve the review-findings storage path: {e}"
            ))
        }
        Ok(p) => p,
    };
    match harness_core::boundary::read_to_string(&path) {
        harness_core::verdict::Determination::Known(None) => ReviewFindingScan::Absent,
        harness_core::verdict::Determination::Known(Some(txt)) => {
            let mut findings = Vec::new();
            for line in txt.lines() {
                if line.is_empty() {
                    continue;
                }
                match serde_json::from_str::<ReviewFinding>(line) {
                    Ok(f) => findings.push(f),
                    // A present-but-unparseable line means the store is
                    // schema-drifted/corrupt; the whole scan is untrustworthy
                    // rather than a silently under-counted `Findings`.
                    Err(e) => {
                        return ReviewFindingScan::Undetermined(format!(
                            "review_findings.jsonl holds an undecodable line: {e}"
                        ))
                    }
                }
            }
            ReviewFindingScan::Findings(findings)
        }
        harness_core::verdict::Determination::Undetermined(reason) => {
            ReviewFindingScan::Undetermined(reason.to_string())
        }
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

/// Read the set of already-bridged finding-ids from bridged_findings.jsonl,
/// BEST-EFFORT: absent, unreadable and partially-undecodable ledgers all read
/// as (or short of) an empty set, i.e. "this finding was never bridged" — which
/// for an idempotency key means "forward it again". Use
/// [`scan_bridged_findings`] where that matters.
pub fn read_bridged_findings(cwd: &Path) -> Result<Vec<String>> {
    Ok(read_jsonl_best_effort::<BridgedFinding>(
        &bridged_findings_path(cwd)?,
        "bridged_findings.jsonl",
    )
    .into_iter()
    .map(|r| r.finding_id)
    .collect())
}

/// Tri-state read of the bridged-finding idempotency ledger (see
/// [`scan_jsonl`]). A never-written ledger genuinely holds no bridged ids
/// (`Known(vec![])`); one that could not be read in full is `Undetermined(why)`
/// rather than a short set that would re-forward findings already in the
/// backlog.
///
/// DIRECTION (t3 judgement — this ledger is NOT in the same class as the
/// finding/rollback/escalation ledgers, so it gets its own answer):
///
/// * collapsing it to EMPTY means "nothing was ever bridged" → every confirmed
///   finding is forwarded AGAIN. The idempotency ledger exists precisely to
///   stop that, so the collapse silently disables the guard it is. The damage
///   is duplicate backlog tasks a human must reconcile by hand — and duplicates
///   are not self-healing.
/// * treating it as "assume everything is already bridged" means this run
///   forwards NOTHING from the stream. Nothing is lost: the source ledgers are
///   append-only and `--to-backlog` re-derives its rows every run, so the next
///   run with a readable ledger forwards exactly the same items.
///
/// One direction needs a human to clean up, the other needs a re-run. So the
/// consumer ([`crate::bridge`]) SKIPS the stream and says so loudly; it must
/// never proceed with an empty already-bridged set.
pub fn scan_bridged_findings(cwd: &Path) -> Result<Determination<Vec<String>>> {
    Ok(
        scan_jsonl::<BridgedFinding>(&bridged_findings_path(cwd)?, "bridged_findings.jsonl")
            .map(|rows| rows.into_iter().map(|r| r.finding_id).collect()),
    )
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
/// bridged_entries.jsonl, BEST-EFFORT (same two-valued contract, and same blind
/// spot, as [`read_bridged_findings`]). Use [`scan_bridged_entries`] where the
/// difference between "never bridged" and "could not tell" matters.
pub fn read_bridged_entries(cwd: &Path) -> Result<Vec<String>> {
    Ok(
        read_jsonl_best_effort::<BridgedEntry>(
            &bridged_entries_path(cwd)?,
            "bridged_entries.jsonl",
        )
        .into_iter()
        .map(|r| r.key)
        .collect(),
    )
}

/// Tri-state read of the bridged-entry idempotency ledger (see [`scan_jsonl`]).
/// Same DIRECTION judgement as [`scan_bridged_findings`]: an undetermined
/// already-bridged set must make the consumer SKIP the stream (re-runnable),
/// never proceed as if nothing had ever been bridged (duplicate backlog tasks
/// a human has to reconcile).
pub fn scan_bridged_entries(cwd: &Path) -> Result<Determination<Vec<String>>> {
    Ok(
        scan_jsonl::<BridgedEntry>(&bridged_entries_path(cwd)?, "bridged_entries.jsonl")
            .map(|rows| rows.into_iter().map(|r| r.key).collect()),
    )
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
/// (append) order.
///
/// Tri-state, deliberately NOT the fail-soft contract of `read_events` /
/// `read_rollbacks`:
///
/// * **absent ledger** → `Known(vec![])`. "No rounds recorded yet" is a real
///   answer, and a fresh checkout must not read as an error.
/// * **unreadable ledger** (permissions, IO error) → `Undetermined`. This arm
///   used to be `Err(_) => Ok(Vec::new())`, which reported "there is no audit
///   history" for a history the process simply could not open.
/// * **unparseable record** → `Undetermined`, via [`audit_round::parse_rounds`].
///   The old loop skipped bad lines silently and returned the survivors, so a
///   single corrupted byte produced a shorter history that looked healthier.
///
/// The three together were measurable on the shipped 0.2.15 binary: corrupting,
/// truncating, or `chmod 000`-ing this file each flipped `overwatch
/// audit-metrics` from `converging: false` to `converging: true` at exit 0.
/// Degrading the evidence improved the verdict — see
/// `crates/overwatch/tests/verdict_monotonicity.rs` and
/// `harness_core::degrade`.
pub fn read_audit_rounds(cwd: &Path) -> Result<Determination<Vec<AuditRound>>> {
    let path = audit_rounds_path(cwd)?;
    match std::fs::read_to_string(&path) {
        Ok(txt) => Ok(audit_round::parse_rounds(&txt)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Determination::Known(Vec::new())),
        Err(e) => Ok(Determination::undetermined(format!(
            "cannot read the audit-round ledger at {}: {e}. The round history is \
             unknown, not empty.",
            path.display()
        ))),
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

/// Read all dispositions from dispositions.jsonl, BEST-EFFORT (same two-valued
/// contract as [`read_review_findings`]): an absent, unreadable or
/// partially-undecodable ledger reads as (or short of) an empty vec, so a
/// finding a human ALREADY dispositioned can come back as undispositioned. Use
/// [`scan_dispositions`] wherever that drives a decision.
pub fn read_dispositions(cwd: &Path) -> Result<Vec<Disposition>> {
    Ok(read_jsonl_best_effort(
        &dispositions_path(cwd)?,
        "dispositions.jsonl",
    ))
}

/// Tri-state read of dispositions.jsonl (see [`scan_jsonl`]): "nobody has
/// dispositioned anything yet" stays a genuine `Known(vec![])`, while a ledger
/// that could not be read in full is `Undetermined(why)`.
pub fn scan_dispositions(cwd: &Path) -> Result<Determination<Vec<Disposition>>> {
    Ok(scan_jsonl(&dispositions_path(cwd)?, "dispositions.jsonl"))
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

/// Load the in-flight changeset registry, keeping a genuinely-absent
/// `active_changesets.json` (nothing in flight — the normal cold start) DISTINCT
/// from one that exists but could not be trusted. Mirrors [`load_leases`], for
/// the same two reasons:
///
/// 1. This registry IS the mid-flight overlap detector. An empty registry means
///    "no other task is touching these files", so folding an unreadable or
///    corrupt file into an empty one answers the conflict question with a
///    confident "no conflict" that was never measured.
/// 2. Every caller here is a read-modify-write that saves the registry back
///    ([`record_changeset_and_detect`], [`mark_changeset_merged`],
///    [`mark_branch_merged`], [`prune_stale_changesets`]). An empty stand-in
///    would be written over the real file, deleting every peer's in-flight
///    changeset.
///
/// So only an absent file yields `Ok(ChangesetRegistry::default())`; unreadable
/// or corrupt yields `Err`, aborting the mutator before it can write anything
/// back. Callers that must not fail on detection already treat an `Err` as "no
/// overlap detected" (only a POSITIVE detection holds a merge) — and now do so
/// without clobbering the registry on the way.
pub fn load_active_changesets(cwd: &Path) -> Result<ChangesetRegistry> {
    let path = active_changesets_path(cwd)?;
    match harness_core::boundary::read_to_string(&path) {
        Determination::Known(None) => Ok(ChangesetRegistry::default()),
        Determination::Known(Some(txt)) => serde_json::from_str(&txt).map_err(|e| {
            anyhow::anyhow!(
                "active_changesets.json present but corrupt at {}: {e}. The in-flight set is \
                 unknown, not empty.",
                path.display()
            )
        }),
        Determination::Undetermined(reason) => anyhow::bail!(
            "active_changesets.json could not be read at {}: {reason}. The in-flight set is \
             unknown, not empty.",
            path.display()
        ),
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

/// Read all runtime-conflict events, BEST-EFFORT: an unreadable or
/// partially-undecodable ledger reads as (or short of) an empty vec, i.e. "no
/// overlap was ever detected". Use [`scan_runtime_conflicts`] where that is
/// acted on.
pub fn read_runtime_conflicts(cwd: &Path) -> Result<Vec<RuntimeConflictEvent>> {
    Ok(read_jsonl_best_effort(
        &runtime_conflicts_path(cwd)?,
        "runtime_conflicts.jsonl",
    ))
}

/// Tri-state read of runtime_conflicts.jsonl (see [`scan_jsonl`]): a
/// never-written ledger is a genuine `Known(vec![])` ("no overlap has been
/// detected"), while one that could not be read in full is `Undetermined(why)`
/// and must not be reported as a clean history.
pub fn scan_runtime_conflicts(cwd: &Path) -> Result<Determination<Vec<RuntimeConflictEvent>>> {
    Ok(scan_jsonl(
        &runtime_conflicts_path(cwd)?,
        "runtime_conflicts.jsonl",
    ))
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
/// out of the hot file never orphans its disposition.
///
/// BEST-EFFORT on BOTH halves, same two-valued contract as
/// [`read_review_findings`]: a missing archive contributes nothing, and an
/// unreadable one — or an undecodable line in either file — contributes nothing
/// too, silently. Consumers that act on the full history must use
/// [`scan_review_findings_all`], which keeps those cases apart.
pub fn read_review_findings_all(cwd: &Path) -> Result<Vec<ReviewFinding>> {
    let mut all = read_review_findings(cwd)?;
    all.extend(read_jsonl_best_effort::<ReviewFinding>(
        &review_findings_archive_path(cwd)?,
        "review_findings_archive.jsonl",
    ));
    Ok(all)
}

/// Tri-state read of the FULL review-finding history (hot store then cold
/// archive), the undetermined-preserving sibling of [`read_review_findings_all`].
///
/// Either half being unreadable — or holding one undecodable line — makes the
/// WHOLE history `Undetermined(why)`: a full-history answer assembled from a
/// half that could not be read is a partial history indistinguishable from a
/// complete one, and the join it feeds (review-metrics latency) would report a
/// finding as never-recorded. Both halves absent is `Known(vec![])`, a genuine
/// "nothing has ever been recorded".
pub fn scan_review_findings_all(cwd: &Path) -> Result<Determination<Vec<ReviewFinding>>> {
    let hot = scan_jsonl::<ReviewFinding>(&review_findings_path(cwd)?, "review_findings.jsonl");
    let archive = scan_jsonl::<ReviewFinding>(
        &review_findings_archive_path(cwd)?,
        "review_findings_archive.jsonl",
    );
    Ok(match (hot, archive) {
        (Determination::Known(mut hot), Determination::Known(archive)) => {
            hot.extend(archive);
            Determination::Known(hot)
        }
        // Forwarded, not re-minted (see `scan_jsonl`).
        (Determination::Undetermined(why), _) | (_, Determination::Undetermined(why)) => {
            Determination::Undetermined(why)
        }
    })
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
/// creates files nor errors). A missing existing archive is likewise a
/// legitimate no-op contribution (first compaction ever). But an existing
/// archive that cannot be read IN FULL — unreadable (permission-denied,
/// non-UTF-8, …) OR holding a line that cannot be decoded — is distinct from
/// absent and must not be treated as if it held (only) the records that came
/// back: this ABORTS with `Err` before either file is rewritten, so an archive
/// that was not fully read is never silently discarded and then clobbered by
/// `write_jsonl_atomic` with a batch missing those records. Never panics.
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
    // Read via the shared tri-state scan so "absent" (legit, first compaction)
    // stays distinct from "could not be read in full" (untrustworthy). Both the
    // unreadable-FILE case and the undecodable-LINE case abort here, because
    // this value is what `write_jsonl_atomic` rewrites the archive FROM: a
    // silently dropped record is not merely uncounted, it is deleted from the
    // cold store on the next line.
    let existing_archive: Vec<ReviewFinding> = match scan_jsonl::<ReviewFinding>(
        &archive_path,
        "review_findings_archive.jsonl",
    ) {
        Determination::Known(v) => v,
        Determination::Undetermined(reason) => {
            anyhow::bail!(
                "cannot compact review findings: the existing archive at {} could not be read in full ({reason}); aborting before any write to avoid discarding it",
                archive_path.display()
            );
        }
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

/// Read all blocked-merge entries, BEST-EFFORT: an unreadable or
/// partially-undecodable ledger reads as (or short of) an empty vec — "no merge
/// is blocked" — which is exactly the answer that lets a held merge through.
/// Use [`scan_merge_conflicts`] where the answer is acted on.
pub fn read_merge_conflicts(cwd: &Path) -> Result<Vec<MergeConflictEntry>> {
    Ok(read_jsonl_best_effort(
        &merge_conflicts_path(cwd)?,
        "merge_conflicts.jsonl",
    ))
}

/// Tri-state read of merge_conflicts.jsonl (see [`scan_jsonl`]): "no merge has
/// ever been blocked" stays a genuine `Known(vec![])`, while a ledger that
/// could not be read in full is `Undetermined(why)`.
pub fn scan_merge_conflicts(cwd: &Path) -> Result<Determination<Vec<MergeConflictEntry>>> {
    Ok(scan_jsonl(
        &merge_conflicts_path(cwd)?,
        "merge_conflicts.jsonl",
    ))
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

/// Read all merge-conflict resolutions, BEST-EFFORT: an unreadable or
/// partially-undecodable ledger reads as (or short of) an empty vec, i.e. "this
/// conflict is still unresolved". Use [`scan_merge_conflict_resolutions`] where
/// that drives a decision.
///
/// DIRECTION (t3 judgement, and why this reader is not simply banned): losing a
/// resolution makes a conflict look STILL OPEN. That over-reports — a human is
/// shown a blocked merge that may already be settled — and it never hides a
/// blocked merge, so the collapse here falls on the conservative side. It is
/// still not free: an over-reported conflict can be re-bridged into the backlog
/// and re-held by a driver. So [`scan_open_merge_conflicts`] does treat this
/// ledger's answer as tri-state, keeps the entries VISIBLE when it is
/// undetermined, and hands the caller a reason to say so — rather than either
/// hiding the entries (the losing direction) or pretending the join was clean.
pub fn read_merge_conflict_resolutions(cwd: &Path) -> Result<Vec<MergeConflictResolution>> {
    Ok(read_jsonl_best_effort(
        &merge_conflict_resolutions_path(cwd)?,
        "merge_conflict_resolutions.jsonl",
    ))
}

/// Tri-state read of merge_conflict_resolutions.jsonl (see [`scan_jsonl`]).
pub fn scan_merge_conflict_resolutions(
    cwd: &Path,
) -> Result<Determination<Vec<MergeConflictResolution>>> {
    Ok(scan_jsonl(
        &merge_conflict_resolutions_path(cwd)?,
        "merge_conflict_resolutions.jsonl",
    ))
}

/// The OPEN blocked-merge set: entries with no resolution (fail-soft read of
/// both streams, joined by `conflict_id`). This is what `review-queue`
/// surfaces as `[merge-conflict]` rows.
///
/// BEST-EFFORT on BOTH streams, and the two collapse in OPPOSITE directions —
/// which is why [`scan_open_merge_conflicts`] exists and why anything that
/// renders or drains this set should call that instead:
///
/// * an unreadable ENTRY ledger reads as "no merge is blocked" (a real work
///   stoppage vanishes — the losing direction);
/// * an unreadable RESOLUTION ledger reads as "nothing is resolved" (a settled
///   conflict is shown again — the conservative direction).
pub fn open_merge_conflicts(cwd: &Path) -> Result<Vec<MergeConflictEntry>> {
    let entries = read_merge_conflicts(cwd)?;
    let resolutions = read_merge_conflict_resolutions(cwd)?;
    Ok(crate::merge_conflict::open_entries(&entries, &resolutions))
}

/// The tri-state answer for the OPEN blocked-merge set, keeping the two ledgers'
/// determinations APART because they fail in opposite directions (see
/// [`scan_open_merge_conflicts`]).
#[derive(Debug)]
pub struct OpenMergeConflictScan {
    /// The open (unresolved) entries. `Undetermined` when the ENTRY ledger
    /// (`merge_conflicts.jsonl`) could not be read in full — the caller must not
    /// render that as "no merge is blocked".
    pub open: Determination<Vec<MergeConflictEntry>>,
    /// `Some(why)` when the RESOLUTION ledger could not be read in full. The
    /// entries above are then joined against NO resolutions, i.e. every entry is
    /// reported open: nothing is hidden, but an already-resolved conflict may be
    /// listed. The caller is expected to SAY this rather than pass the join off
    /// as clean.
    pub resolutions_undetermined: Option<String>,
}

/// Tri-state [`open_merge_conflicts`]: the sanctioned reader for the review
/// surface and the backlog drain.
///
/// The direction judgement, written down because the two halves are NOT
/// symmetric:
///
/// * ENTRY ledger undetermined → `open: Undetermined`. Reading it as an empty
///   set is "no merge is blocked", the exact answer that lets a held merge pass
///   unnoticed. The caller must omit-and-announce, never claim zero.
/// * RESOLUTION ledger undetermined → the entries are still returned, joined
///   against an EMPTY resolution set, and `resolutions_undetermined` carries
///   why. Dropping to "nothing is resolved" over-reports (a resolved conflict
///   reappears) and cannot hide a blocked merge, so withholding the whole
///   source here would trade a conservative error for the losing one.
pub fn scan_open_merge_conflicts(cwd: &Path) -> Result<OpenMergeConflictScan> {
    let (resolutions, resolutions_undetermined) = match scan_merge_conflict_resolutions(cwd)? {
        Determination::Known(rows) => (rows, None),
        Determination::Undetermined(why) => (Vec::new(), Some(why.as_str().to_string())),
    };
    let open = scan_merge_conflicts(cwd)?
        .map(|entries| crate::merge_conflict::open_entries(&entries, &resolutions));
    Ok(OpenMergeConflictScan {
        open,
        resolutions_undetermined,
    })
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
        let _guard = home_lock();
        let (dir, prev_home) = scan_test_home();
        assert!(matches!(scan_violations(&dir), ViolationScan::Absent));
        restore_home(prev_home);
    }

    // A clean, fully-parseable registry scans as `Events` with every line — the
    // authoritative count. Distinct from `Absent` (an empty file present but
    // holding no events still yields `Events(empty)`, never `Undetermined`).
    #[test]
    fn scan_violations_events_when_all_lines_parse() {
        let _guard = home_lock();
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
        let _guard = home_lock();
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
        let _guard = home_lock();
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

    // A corrupt (present but unparseable) leases.json must surface as an Err
    // from `load_leases`, not silently fold into an empty registry. RED (before
    // the fix): the raw `std::fs::read_to_string` + `unwrap_or_default()` match
    // collapses BOTH "unreadable" and "corrupt" into `Ok(LeaseRegistry::default())`,
    // identical to a genuinely-absent file — a read-modify-write caller then
    // clobbers every other session's lease with an empty registry (mass
    // double-claim). GREEN: the boundary tri-state read distinguishes
    // Known(None) (absent → legit empty registry) from Known(Some(txt)) with a
    // corrupt parse, and from Undetermined (unreadable) — both of the latter
    // two become Err.
    #[test]
    fn load_leases_surfaces_corrupt_or_unreadable_registry_as_err() {
        let _guard = home_lock();
        let prev_home = std::env::var_os("HOME");
        let dir = std::env::temp_dir().join(format!(
            "overwatch-store-test-corrupt-leases-{}-{}",
            std::process::id(),
            now()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("HOME", &dir);

        // A genuinely-absent leases.json is still a legitimate cold-start empty
        // registry, not an error.
        assert!(
            load_leases(&dir).unwrap().is_empty(),
            "a missing leases.json must still yield an empty registry"
        );

        // Now corrupt it: present bytes that are valid UTF-8 but not valid JSON.
        let path = leases_path(&dir).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"{ this is not valid json : : [").unwrap();

        assert!(
            load_leases(&dir).is_err(),
            "a corrupt leases.json must surface as Err, not silently fold to an empty registry"
        );

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

        let _guard = home_lock();
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
        let _guard = home_lock();
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
        let _guard = home_lock();
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
        let _guard = home_lock();
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

    // A pre-existing archive that EXISTS but is UNREADABLE (non-UTF-8 bytes,
    // independent of and root-safe unlike permission bits) must abort the
    // compaction with an Err BEFORE any write, rather than being silently
    // treated as absent (empty Vec) and then overwritten with only the
    // newly-archived batch — which would destroy every previously-archived
    // finding. RED (before the fix): the raw `std::fs::read_to_string(&
    // archive_path)` match collapses the InvalidData error to `Vec::new()`
    // in its `Err(_)` arm, so `compact_review_findings` returns `Ok` and
    // `write_jsonl_atomic` clobbers the archive file, losing the non-UTF-8
    // bytes. GREEN: the boundary tri-state read distinguishes Undetermined
    // (unreadable) from Known(None) (absent) and bails out before either
    // `write_jsonl_atomic` call runs, leaving the archive bytes untouched.
    //
    // ANTI-VACUITY, and where it actually lives (third-party audit, t4): this
    // test asserts only that compaction ABORTS, so an implementation that
    // aborted unconditionally would satisfy it. Measured by mutation
    // (`compact_review_findings` made to `bail!` on every call): this test and
    // its `..._undecodable_archive_line` sibling both stayed GREEN, while 13
    // others went red — `concurrent_append_survives_compact_review_findings`
    // here and `compact_findings_archives_resolved_and_keeps_hot_bounded_to_open`
    // in `tests/compact_findings_cli.rs` are the controls that carry the
    // opposite polarity. They are in different files, so the pairing is named
    // here rather than left to be rediscovered.
    #[test]
    fn compact_review_findings_aborts_on_unreadable_archive() {
        let _guard = home_lock();
        let prev_home = std::env::var_os("HOME");
        let dir = std::env::temp_dir().join(format!(
            "overwatch-compact-unreadable-archive-{}-{}",
            std::process::id(),
            now()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("HOME", &dir);

        // Seed the hot store with a resolved finding so compaction would want
        // to archive it (and would touch the archive file) if it ran.
        append_review_finding(&dir, &finding("r1", 1)).unwrap();
        append_bridged_finding(&dir, "r1").unwrap();

        // Corrupt the archive with non-UTF-8 bytes: std::fs::read_to_string
        // returns Err(InvalidData) for this, which the shared boundary reader
        // classifies as Undetermined (exists but unreadable) rather than
        // Known(None) (absent).
        let archive_path = review_findings_archive_path(&dir).unwrap();
        std::fs::create_dir_all(archive_path.parent().unwrap()).unwrap();
        let corrupt_bytes: &[u8] = &[0xFF, 0xFE, 0xFF];
        std::fs::write(&archive_path, corrupt_bytes).unwrap();

        let result = compact_review_findings(&dir);
        assert!(
            result.is_err(),
            "compaction must abort (Err) when the existing archive is unreadable, not silently discard it"
        );

        // The archive on disk must be UNCHANGED: the destructive overwrite
        // (discarding the unreadable archive as an empty Vec, then writing
        // only the newly-archived batch) must not have happened.
        let bytes_after = std::fs::read(&archive_path).unwrap();
        assert_eq!(
            bytes_after, corrupt_bytes,
            "the unreadable archive must survive untouched, not be overwritten"
        );

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
        let _guard = home_lock();
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

        let _guard = home_lock();
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
        let _guard = home_lock();
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
        let _guard = home_lock();
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
        let _guard = home_lock();
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

    // ── Tri-state ledger readers ─────────────────────────────────────────────
    //
    // Every `scan_*` reader must answer in THREE parts, never two:
    //
    //   absent ledger          -> `Known(empty)`  a genuine measurement of zero
    //   clean ledger           -> `Known(rows)`   ANTI-VACUITY CONTROL
    //   one undecodable line   -> `Undetermined`  never a silently-short `Known`
    //   present but unreadable -> `Undetermined`  never an empty `Known`
    //
    // The middle two arms are a matched pair on purpose. An implementation that
    // answered `Undetermined` for EVERYTHING would satisfy the corrupt and
    // unreadable arms while destroying the signal (every reader would go blind),
    // so each test asserts the clean ledger still reads its rows back; and an
    // absent ledger must stay a real, usable empty or "undetermined" becomes the
    // permanent answer and stops meaning anything.

    /// Take the crate-wide `$HOME` lock, recovering from POISON.
    ///
    /// A test that fails an assertion panics while holding [`HOME_ENV_LOCK`],
    /// which poisons it for every later `$HOME`-sandboxing test in the same
    /// process. `.lock().unwrap()` then turns each of those into a
    /// `PoisonError` failure that says nothing about the property it checks —
    /// one real red is reported as twenty, and the true verdict of these tests
    /// is buried. The mutex guards a process-global env var, not invariants of
    /// a data structure, and each test sets `$HOME` itself before touching the
    /// store, so a poisoned lock leaves nothing inconsistent to protect against.
    fn home_lock() -> std::sync::MutexGuard<'static, ()> {
        HOME_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A fresh sandboxed `$HOME` (so `storage_root` resolves into a temp dir and
    /// the real `~/.overwatch` is never touched). Callers MUST hold
    /// [`HOME_ENV_LOCK`] (via [`home_lock`]) and restore the previous `$HOME`
    /// themselves.
    fn fresh_ledger_home(tag: &str) -> std::path::PathBuf {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "overwatch-ledger-{tag}-{}-{}-{}",
            std::process::id(),
            now_unix_nanos(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("HOME", &dir);
        dir
    }

    /// Append a line that is present and non-blank but cannot be decoded
    /// (schema drift / corruption).
    fn append_undecodable_line(path: &std::path::Path) {
        use std::io::Write as _;
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p).unwrap();
        }
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        writeln!(f, "{{not-valid-json-schema-drift").unwrap();
    }

    /// Make `path` EXIST but be unreadable as text: non-UTF-8 bytes make
    /// `read_to_string` return `InvalidData`, which the shared boundary
    /// classifies as `Undetermined` (present but opaque) rather than
    /// `Known(None)` (absent). Deterministic for every user, unlike a
    /// `chmod 000` (a no-op when running as root).
    fn write_unreadable(path: &std::path::Path) {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p).unwrap();
        }
        std::fs::write(path, [0xFFu8, 0xFE, 0xFD]).unwrap();
    }

    #[track_caller]
    fn assert_known_len<T: std::fmt::Debug>(d: Determination<Vec<T>>, want: usize, what: &str) {
        // `None` stands for "answered Undetermined", so the assertion prints the
        // whole determination either way. (Written without `panic!` because the
        // workspace denies `clippy::panic` for this crate, tests included.)
        let got = match &d {
            Determination::Known(v) => Some(v.len()),
            Determination::Undetermined(_) => None,
        };
        assert_eq!(
            got,
            Some(want),
            "{what}: expected Known({want}) — a ledger that WAS read is a measurement — got {d:?}"
        );
    }

    #[track_caller]
    fn assert_undetermined<T: std::fmt::Debug>(d: Determination<Vec<T>>, what: &str) {
        assert!(
            matches!(d, Determination::Undetermined(_)),
            "{what}: a ledger that could not be read IN FULL must not answer Known — \
             \"could not read\" is not \"nothing was recorded\" — got {d:?}"
        );
    }

    fn lifecycle_event(key: &str, ts: i64) -> LifecycleEvent {
        LifecycleEvent::started(
            key.to_string(),
            "title".to_string(),
            "sess".to_string(),
            "run".to_string(),
            ts,
        )
    }

    fn rollback_event(plugin: &str, ts: i64) -> RollbackEvent {
        RollbackEvent::new(
            plugin.to_string(),
            Some("0.1.0".to_string()),
            "0.1.1".to_string(),
            0,
            crate::rollback::RollbackReason::Raw,
            ts,
            None,
        )
    }

    fn disposition(finding_id: &str) -> Disposition {
        use crate::disposition::DispositionVerdict;
        Disposition {
            finding_id: finding_id.to_string(),
            verdict: DispositionVerdict::Confirmed,
            reviewer: "tester".to_string(),
            resolved_ts: 1_700_000_000,
        }
    }

    fn runtime_conflict_event(task_a: &str, ts: i64) -> RuntimeConflictEvent {
        RuntimeConflictEvent {
            run_id: "run".to_string(),
            task_key_a: task_a.to_string(),
            task_key_b: "other".to_string(),
            overlapping_files: vec!["a.rs".to_string()],
            base_ref: "base".to_string(),
            session_id: "sess".to_string(),
            ts,
            detail: "overlap".to_string(),
        }
    }

    fn merge_conflict_entry(conflict_id: &str, ts: i64) -> MergeConflictEntry {
        MergeConflictEntry {
            conflict_id: conflict_id.to_string(),
            origin: crate::merge_conflict::ConflictOrigin::RuntimeOverlap,
            run_id: "run".to_string(),
            branch: "condukt/t1".to_string(),
            default_branch: "main".to_string(),
            base_ref: "base".to_string(),
            conflicted_files: vec!["a.rs".to_string()],
            diff_ours: String::new(),
            diff_theirs: String::new(),
            ts,
        }
    }

    fn merge_conflict_resolution(conflict_id: &str, ts: i64) -> MergeConflictResolution {
        MergeConflictResolution {
            conflict_id: conflict_id.to_string(),
            choice: crate::merge_conflict::ResolveChoice::Theirs,
            decided_by: crate::merge_conflict::DecidedBy::Policy,
            note: None,
            ts,
        }
    }

    #[test]
    fn events_ledger_scan_is_tri_state() {
        let _guard = home_lock();
        let prev_home = std::env::var_os("HOME");

        let dir = fresh_ledger_home("events-absent");
        assert_known_len(scan_events(&dir).unwrap(), 0, "absent events.jsonl");

        let dir = fresh_ledger_home("events-clean");
        append_event(&dir, &lifecycle_event("k1", 1)).unwrap();
        append_event(&dir, &lifecycle_event("k2", 2)).unwrap();
        // ANTI-VACUITY: a readable ledger is still a measurement.
        assert_known_len(scan_events(&dir).unwrap(), 2, "clean events.jsonl");

        append_undecodable_line(&events_path(&dir).unwrap());
        assert_undetermined(scan_events(&dir).unwrap(), "events.jsonl, undecodable line");
        assert_eq!(
            read_events(&dir).unwrap().len(),
            2,
            "the legacy best-effort reader keeps its documented behaviour (t3 migrates callers)"
        );

        let dir = fresh_ledger_home("events-opaque");
        write_unreadable(&events_path(&dir).unwrap());
        assert_undetermined(scan_events(&dir).unwrap(), "unreadable events.jsonl");

        restore_home(prev_home);
    }

    #[test]
    fn rollbacks_ledger_scan_is_tri_state() {
        let _guard = home_lock();
        let prev_home = std::env::var_os("HOME");

        let dir = fresh_ledger_home("rollbacks-absent");
        assert_known_len(scan_rollbacks(&dir).unwrap(), 0, "absent rollbacks.jsonl");

        let dir = fresh_ledger_home("rollbacks-clean");
        append_rollback(&dir, &rollback_event("blastguard", 1)).unwrap();
        assert_known_len(scan_rollbacks(&dir).unwrap(), 1, "clean rollbacks.jsonl");

        append_undecodable_line(&rollbacks_path(&dir).unwrap());
        assert_undetermined(
            scan_rollbacks(&dir).unwrap(),
            "rollbacks.jsonl, undecodable line",
        );

        let dir = fresh_ledger_home("rollbacks-opaque");
        write_unreadable(&rollbacks_path(&dir).unwrap());
        assert_undetermined(scan_rollbacks(&dir).unwrap(), "unreadable rollbacks.jsonl");

        restore_home(prev_home);
    }

    #[test]
    fn bridged_findings_ledger_scan_is_tri_state() {
        let _guard = home_lock();
        let prev_home = std::env::var_os("HOME");

        let dir = fresh_ledger_home("bridged-findings-absent");
        assert_known_len(
            scan_bridged_findings(&dir).unwrap(),
            0,
            "absent bridged_findings.jsonl",
        );

        let dir = fresh_ledger_home("bridged-findings-clean");
        append_bridged_finding(&dir, "f-1").unwrap();
        append_bridged_finding(&dir, "f-2").unwrap();
        assert_known_len(
            scan_bridged_findings(&dir).unwrap(),
            2,
            "clean bridged_findings.jsonl",
        );

        append_undecodable_line(&bridged_findings_path(&dir).unwrap());
        assert_undetermined(
            scan_bridged_findings(&dir).unwrap(),
            "bridged_findings.jsonl, undecodable line",
        );

        let dir = fresh_ledger_home("bridged-findings-opaque");
        write_unreadable(&bridged_findings_path(&dir).unwrap());
        assert_undetermined(
            scan_bridged_findings(&dir).unwrap(),
            "unreadable bridged_findings.jsonl",
        );

        restore_home(prev_home);
    }

    #[test]
    fn bridged_entries_ledger_scan_is_tri_state() {
        let _guard = home_lock();
        let prev_home = std::env::var_os("HOME");

        let dir = fresh_ledger_home("bridged-entries-absent");
        assert_known_len(
            scan_bridged_entries(&dir).unwrap(),
            0,
            "absent bridged_entries.jsonl",
        );

        let dir = fresh_ledger_home("bridged-entries-clean");
        append_bridged_entry(&dir, "systemic:sig").unwrap();
        assert_known_len(
            scan_bridged_entries(&dir).unwrap(),
            1,
            "clean bridged_entries.jsonl",
        );

        append_undecodable_line(&bridged_entries_path(&dir).unwrap());
        assert_undetermined(
            scan_bridged_entries(&dir).unwrap(),
            "bridged_entries.jsonl, undecodable line",
        );

        let dir = fresh_ledger_home("bridged-entries-opaque");
        write_unreadable(&bridged_entries_path(&dir).unwrap());
        assert_undetermined(
            scan_bridged_entries(&dir).unwrap(),
            "unreadable bridged_entries.jsonl",
        );

        restore_home(prev_home);
    }

    #[test]
    fn dispositions_ledger_scan_is_tri_state() {
        let _guard = home_lock();
        let prev_home = std::env::var_os("HOME");

        let dir = fresh_ledger_home("dispositions-absent");
        assert_known_len(
            scan_dispositions(&dir).unwrap(),
            0,
            "absent dispositions.jsonl",
        );

        let dir = fresh_ledger_home("dispositions-clean");
        append_disposition(&dir, &disposition("f-1")).unwrap();
        assert_known_len(
            scan_dispositions(&dir).unwrap(),
            1,
            "clean dispositions.jsonl",
        );

        append_undecodable_line(&dispositions_path(&dir).unwrap());
        assert_undetermined(
            scan_dispositions(&dir).unwrap(),
            "dispositions.jsonl, undecodable line",
        );

        let dir = fresh_ledger_home("dispositions-opaque");
        write_unreadable(&dispositions_path(&dir).unwrap());
        assert_undetermined(
            scan_dispositions(&dir).unwrap(),
            "unreadable dispositions.jsonl",
        );

        restore_home(prev_home);
    }

    #[test]
    fn runtime_conflicts_ledger_scan_is_tri_state() {
        let _guard = home_lock();
        let prev_home = std::env::var_os("HOME");

        let dir = fresh_ledger_home("runtime-conflicts-absent");
        assert_known_len(
            scan_runtime_conflicts(&dir).unwrap(),
            0,
            "absent runtime_conflicts.jsonl",
        );

        let dir = fresh_ledger_home("runtime-conflicts-clean");
        append_runtime_conflict(&dir, &runtime_conflict_event("runX/ta", 1)).unwrap();
        assert_known_len(
            scan_runtime_conflicts(&dir).unwrap(),
            1,
            "clean runtime_conflicts.jsonl",
        );

        append_undecodable_line(&runtime_conflicts_path(&dir).unwrap());
        assert_undetermined(
            scan_runtime_conflicts(&dir).unwrap(),
            "runtime_conflicts.jsonl, undecodable line",
        );

        let dir = fresh_ledger_home("runtime-conflicts-opaque");
        write_unreadable(&runtime_conflicts_path(&dir).unwrap());
        assert_undetermined(
            scan_runtime_conflicts(&dir).unwrap(),
            "unreadable runtime_conflicts.jsonl",
        );

        restore_home(prev_home);
    }

    #[test]
    fn merge_conflicts_ledger_scan_is_tri_state() {
        let _guard = home_lock();
        let prev_home = std::env::var_os("HOME");

        let dir = fresh_ledger_home("merge-conflicts-absent");
        assert_known_len(
            scan_merge_conflicts(&dir).unwrap(),
            0,
            "absent merge_conflicts.jsonl",
        );

        let dir = fresh_ledger_home("merge-conflicts-clean");
        append_merge_conflict(&dir, &merge_conflict_entry("c-1", 1)).unwrap();
        assert_known_len(
            scan_merge_conflicts(&dir).unwrap(),
            1,
            "clean merge_conflicts.jsonl",
        );

        append_undecodable_line(&merge_conflicts_path(&dir).unwrap());
        assert_undetermined(
            scan_merge_conflicts(&dir).unwrap(),
            "merge_conflicts.jsonl, undecodable line",
        );

        let dir = fresh_ledger_home("merge-conflicts-opaque");
        write_unreadable(&merge_conflicts_path(&dir).unwrap());
        assert_undetermined(
            scan_merge_conflicts(&dir).unwrap(),
            "unreadable merge_conflicts.jsonl",
        );

        restore_home(prev_home);
    }

    #[test]
    fn merge_conflict_resolutions_ledger_scan_is_tri_state() {
        let _guard = home_lock();
        let prev_home = std::env::var_os("HOME");

        let dir = fresh_ledger_home("merge-resolutions-absent");
        assert_known_len(
            scan_merge_conflict_resolutions(&dir).unwrap(),
            0,
            "absent merge_conflict_resolutions.jsonl",
        );

        let dir = fresh_ledger_home("merge-resolutions-clean");
        append_merge_conflict_resolution(&dir, &merge_conflict_resolution("c-1", 1)).unwrap();
        assert_known_len(
            scan_merge_conflict_resolutions(&dir).unwrap(),
            1,
            "clean merge_conflict_resolutions.jsonl",
        );

        append_undecodable_line(&merge_conflict_resolutions_path(&dir).unwrap());
        assert_undetermined(
            scan_merge_conflict_resolutions(&dir).unwrap(),
            "merge_conflict_resolutions.jsonl, undecodable line",
        );

        let dir = fresh_ledger_home("merge-resolutions-opaque");
        write_unreadable(&merge_conflict_resolutions_path(&dir).unwrap());
        assert_undetermined(
            scan_merge_conflict_resolutions(&dir).unwrap(),
            "unreadable merge_conflict_resolutions.jsonl",
        );

        restore_home(prev_home);
    }

    /// The OPEN blocked-merge join reads TWO ledgers that fail in OPPOSITE
    /// directions, and `scan_open_merge_conflicts` must keep them apart:
    ///
    /// * entries undetermined → the whole answer is undetermined (reading it as
    ///   "no merge is blocked" is the losing direction);
    /// * resolutions undetermined → the entries are still returned (nothing is
    ///   hidden) but the caller is TOLD, because the filter did not run.
    ///
    /// The clean and absent arms are the anti-vacuity controls: an
    /// implementation that answered undetermined for everything would satisfy
    /// the two failure arms while blinding the review surface.
    #[test]
    fn open_merge_conflict_scan_separates_the_two_ledgers_directions() {
        let _guard = home_lock();
        let prev_home = std::env::var_os("HOME");

        // Absent: nothing was ever recorded — a real, trustworthy empty.
        let dir = fresh_ledger_home("open-mc-absent");
        let scan = scan_open_merge_conflicts(&dir).unwrap();
        assert_known_len(scan.open, 0, "absent merge_conflicts.jsonl");
        assert!(scan.resolutions_undetermined.is_none());

        // Clean: one entry, no resolution → one OPEN conflict (control).
        let dir = fresh_ledger_home("open-mc-clean");
        append_merge_conflict(&dir, &merge_conflict_entry("c-1", 1)).unwrap();
        let scan = scan_open_merge_conflicts(&dir).unwrap();
        assert_known_len(scan.open, 1, "one unresolved conflict");
        assert!(scan.resolutions_undetermined.is_none());

        // Clean + resolved: the filter still works (control — the join must not
        // become a no-op just because it grew a third answer).
        append_merge_conflict_resolution(&dir, &merge_conflict_resolution("c-1", 2)).unwrap();
        let scan = scan_open_merge_conflicts(&dir).unwrap();
        assert_known_len(scan.open, 0, "the resolved conflict drops out");
        assert!(scan.resolutions_undetermined.is_none());

        // ENTRY ledger undetermined → the whole set is undetermined.
        let dir = fresh_ledger_home("open-mc-entries-corrupt");
        append_merge_conflict(&dir, &merge_conflict_entry("c-2", 1)).unwrap();
        append_undecodable_line(&merge_conflicts_path(&dir).unwrap());
        let scan = scan_open_merge_conflicts(&dir).unwrap();
        assert_undetermined(scan.open, "merge_conflicts.jsonl with an undecodable line");

        // RESOLUTION ledger undetermined → entries STILL returned, and said so.
        let dir = fresh_ledger_home("open-mc-resolutions-corrupt");
        append_merge_conflict(&dir, &merge_conflict_entry("c-3", 1)).unwrap();
        write_unreadable(&merge_conflict_resolutions_path(&dir).unwrap());
        let scan = scan_open_merge_conflicts(&dir).unwrap();
        assert_known_len(
            scan.open,
            1,
            "an unreadable RESOLUTION ledger must not hide the conflict — it \
             over-reports, which is the conservative direction",
        );
        assert!(
            scan.resolutions_undetermined.is_some(),
            "the un-run filter must be reported, not passed off as a clean join"
        );

        restore_home(prev_home);
    }

    /// The full-history (hot + cold archive) view must be tri-state on BOTH
    /// halves: the archive read was a bare `if let Ok(txt)` that dropped an
    /// unreadable archive entirely, and its decode loop dropped bad lines.
    #[test]
    fn review_findings_all_scan_is_tri_state_over_hot_and_archive() {
        let _guard = home_lock();
        let prev_home = std::env::var_os("HOME");

        let dir = fresh_ledger_home("findings-all-absent");
        assert_known_len(
            scan_review_findings_all(&dir).unwrap(),
            0,
            "absent hot store and archive",
        );

        let dir = fresh_ledger_home("findings-all-clean");
        append_review_finding(&dir, &finding("a", 1)).unwrap();
        let archive = review_findings_archive_path(&dir).unwrap();
        std::fs::create_dir_all(archive.parent().unwrap()).unwrap();
        std::fs::write(
            &archive,
            format!("{}\n", serde_json::to_string(&finding("b", 2)).unwrap()),
        )
        .unwrap();
        assert_known_len(
            scan_review_findings_all(&dir).unwrap(),
            2,
            "clean hot store plus archive",
        );

        // An undecodable ARCHIVE line makes the full history untrustworthy.
        append_undecodable_line(&archive);
        assert_undetermined(
            scan_review_findings_all(&dir).unwrap(),
            "archive with an undecodable line",
        );

        // So does an undecodable HOT line (the other half of the same join).
        let dir = fresh_ledger_home("findings-all-hot-corrupt");
        append_review_finding(&dir, &finding("a", 1)).unwrap();
        append_undecodable_line(&review_findings_path(&dir).unwrap());
        assert_undetermined(
            scan_review_findings_all(&dir).unwrap(),
            "hot store with an undecodable line",
        );

        // And an archive that exists but cannot be read at all.
        let dir = fresh_ledger_home("findings-all-opaque");
        append_review_finding(&dir, &finding("a", 1)).unwrap();
        write_unreadable(&review_findings_archive_path(&dir).unwrap());
        assert_undetermined(
            scan_review_findings_all(&dir).unwrap(),
            "unreadable archive",
        );

        restore_home(prev_home);
    }

    /// `load_active_changesets` feeds mid-flight overlap detection AND a
    /// read-modify-write that saves the registry back. Folding an unreadable or
    /// corrupt registry into an empty one therefore did two things at once:
    /// reported "no in-flight work to conflict with", and then clobbered every
    /// peer's changeset on the next save. Mirrors `load_leases`: only a
    /// genuinely-absent file is an empty registry; anything else is `Err`.
    #[test]
    fn load_active_changesets_separates_absent_from_unreadable() {
        let _guard = home_lock();
        let prev_home = std::env::var_os("HOME");

        let dir = fresh_ledger_home("changesets-absent");
        assert!(
            load_active_changesets(&dir).unwrap().is_empty(),
            "an absent registry is a genuine cold start, not an error"
        );

        // ANTI-VACUITY: a readable registry still loads its entries.
        let dir = fresh_ledger_home("changesets-clean");
        record_changeset_and_detect(&dir, &changeset("runX/ta", &["a.rs"], now())).unwrap();
        assert_eq!(
            load_active_changesets(&dir).unwrap().len(),
            1,
            "a readable registry is a measurement and must load"
        );

        let dir = fresh_ledger_home("changesets-corrupt");
        let path = active_changesets_path(&dir).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{not-valid-json").unwrap();
        assert!(
            load_active_changesets(&dir).is_err(),
            "a corrupt registry must not load as an empty one — the RMW would clobber every peer"
        );

        let dir = fresh_ledger_home("changesets-opaque");
        write_unreadable(&active_changesets_path(&dir).unwrap());
        assert!(
            load_active_changesets(&dir).is_err(),
            "an unreadable registry must not load as an empty one"
        );

        restore_home(prev_home);
    }

    /// Compaction REWRITES the archive from what it decoded. Silently skipping
    /// an undecodable archive line therefore deletes it from the cold store for
    /// good — the same destructive shape as the already-fixed unreadable-archive
    /// case (`compact_review_findings_aborts_on_unreadable_archive`), one mirror
    /// over.
    #[test]
    // Same anti-vacuity caveat as `..._aborts_on_unreadable_archive` above: the
    // control that refuses an always-aborting compaction is in another test.
    fn compact_review_findings_aborts_on_undecodable_archive_line() {
        let _guard = home_lock();
        let prev_home = std::env::var_os("HOME");

        let dir = fresh_ledger_home("compact-undecodable-archive");
        append_review_finding(&dir, &finding("r1", 1)).unwrap();
        append_bridged_finding(&dir, "r1").unwrap();

        let archive_path = review_findings_archive_path(&dir).unwrap();
        std::fs::create_dir_all(archive_path.parent().unwrap()).unwrap();
        let archive_before = format!(
            "{}\nnot-valid-json-schema-drift\n",
            serde_json::to_string(&finding("old", 0)).unwrap()
        );
        std::fs::write(&archive_path, &archive_before).unwrap();

        assert!(
            compact_review_findings(&dir).is_err(),
            "compaction must abort when an existing archive line cannot be decoded, \
             rather than rewrite the archive without it"
        );
        assert_eq!(
            std::fs::read_to_string(&archive_path).unwrap(),
            archive_before,
            "the archive must survive untouched"
        );

        restore_home(prev_home);
    }
}
