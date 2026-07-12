/// The unified human review surface: `overwatch review-queue`.
///
/// Three streams that were previously separate — systemic gate violations, canary
/// rollback events, and AI/adversarial review findings — are merged into ONE
/// risk-ordered list, each row carrying a `kind` discriminator so a human (or a
/// tool) can tell the source types apart. Ordering is **severity-first**
/// (highest normalized [`Severity`] at the top, newest `ts` breaking ties
/// within a severity band), documented once here.
///
/// The merge itself is a pure, deterministic function over the three input
/// slices ([`build_queue`]); the CLI shell ([`run`]) reads the stores fail-soft
/// (a missing/empty source contributes nothing rather than erroring the whole
/// command) and renders either a human-readable list or a JSON array.
use crate::review_finding::ReviewFinding;
use crate::rollback::RollbackEvent;
use crate::store;
use crate::violation::{self, RecurrencePolicy, SignatureRecurrence};
use anyhow::Result;
use serde::{Deserialize, Serialize};

/// The source type of a review-queue entry. Serialized as the `kind`
/// discriminator on each JSON row.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum EntryKind {
    /// A systemic (cross-task recurring) gate-violation signature.
    Systemic,
    /// A canary health-gate rollback event.
    Rollback,
    /// An AI/adversarial review finding.
    AiFinding,
}

impl EntryKind {
    /// Short human tag shown in the default (non-JSON) output.
    pub fn tag(self) -> &'static str {
        match self {
            EntryKind::Systemic => "systemic",
            EntryKind::Rollback => "rollback",
            EntryKind::AiFinding => "ai-finding",
        }
    }
}

/// Normalized risk severity used to rank the review queue. Variants are
/// declared low-to-high so the derived `Ord`/`PartialOrd` gives the natural
/// risk ranking (`Severity::High > Severity::Medium > Severity::Low`),
/// letting the sort in [`build_queue`] compare severities directly.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
pub enum Severity {
    Low,
    Medium,
    High,
}

/// Normalize a free-text severity string (case-insensitive) into a
/// [`Severity`] ordinal. Unrecognized/garbage text — and, by extension, a
/// missing severity — defaults to `Medium` rather than `Low`: silently
/// treating "unknown" as low-risk would bury an item whose severity simply
/// wasn't reported cleanly, undermining the whole point of risk ranking.
pub fn normalize_severity(raw: &str) -> Severity {
    match raw.trim().to_ascii_lowercase().as_str() {
        "high" => Severity::High,
        "medium" | "med" => Severity::Medium,
        "low" => Severity::Low,
        _ => Severity::Medium,
    }
}

/// One row in the unified review queue.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewQueueEntry {
    /// Which of the three streams this row came from.
    pub kind: EntryKind,
    /// Normalized risk severity, used as the primary sort key (High-first).
    pub severity: Severity,
    /// Unix timestamp used as the secondary (tiebreak) ordering.
    pub ts: i64,
    /// A short human-readable summary of the item.
    pub summary: String,
    /// The key identifier for this row: the violation signature, the rolled-back
    /// plugin, or the finding id — whatever most identifies the item.
    pub identifier: String,
}

/// Deduplicate AI findings by `finding_id`, keeping only the **latest** (`ts`)
/// record per id.
///
/// The Continuous-Audit loop re-records confirmed findings every round; a
/// finding that persists across rounds is `record-finding`ed repeatedly with the
/// same `finding_id`. Without this collapse the review queue would grow one row
/// per round for the *same* finding, so an auto-populating loop becomes unusable
/// noise. Keeping the newest record means the surfaced row reflects the finding's
/// most recent state (severity/summary can be revised between rounds). On an
/// exact `ts` tie the later record in `findings` wins ("last write wins" at the
/// same instant), which is deterministic because the input slice order is stable
/// (append order of `review_findings.jsonl`). Only the AI-findings stream is
/// touched — the systemic and rollback streams never pass through here.
pub(crate) fn dedup_findings(findings: &[ReviewFinding]) -> Vec<ReviewFinding> {
    use std::collections::HashMap;
    let mut best: HashMap<&str, ReviewFinding> = HashMap::new();
    for f in findings {
        match best.get(f.finding_id.as_str()) {
            // Keep the existing record only if it is strictly newer; otherwise
            // insert (new id) or replace (>= ts → newest, ties favour later input).
            Some(existing) if existing.ts > f.ts => {}
            _ => {
                best.insert(f.finding_id.as_str(), f.clone());
            }
        }
    }
    best.into_values().collect()
}

/// Deterministically merge the three sources into one **severity-first**
/// queue (highest risk at the top, newest-first within a severity band).
///
/// * systemic violations are taken from `systemic` (already filtered to
///   `is_systemic` by the caller), keyed by signature, timestamped at
///   `last_seen`, and always ranked `Severity::High` (a real, already-
///   happening cross-task problem);
/// * rollbacks are keyed by plugin, timestamped at their `ts`, and always
///   ranked `Severity::High` (a shipped regression the fleet caught);
/// * AI findings are keyed by `finding_id`, timestamped at their `ts`, and
///   ranked via [`normalize_severity`] on their free-text `severity` field
///   (missing/unrecognized → `Severity::Medium`); findings sharing a
///   `finding_id` are first collapsed to their newest record by
///   [`dedup_findings`] so a finding recurring across audit rounds is ONE row.
///
/// A missing/empty source simply contributes no rows. Ties on
/// `(severity, ts)` are broken deterministically by (kind, identifier) so the
/// ordering is stable and reproducible.
pub fn build_queue(
    systemic: &[SignatureRecurrence],
    rollbacks: &[RollbackEvent],
    findings: &[ReviewFinding],
) -> Vec<ReviewQueueEntry> {
    let mut rows: Vec<ReviewQueueEntry> = Vec::new();

    for r in systemic {
        rows.push(ReviewQueueEntry {
            kind: EntryKind::Systemic,
            // A systemic recurring violation is a real, already-happening
            // cross-task problem (it passed `is_systemic` recurrence
            // filtering upstream), not a hypothetical risk — always High.
            severity: Severity::High,
            ts: r.last_seen,
            summary: format!(
                "systemic signature recurred {}x across {} task(s)/{} session(s)",
                r.occurrences, r.distinct_tasks, r.distinct_sessions
            ),
            identifier: r.signature.clone(),
        });
    }

    for rb in rollbacks {
        let from = rb.from_version.as_deref().unwrap_or("(new)");
        rows.push(ReviewQueueEntry {
            kind: EntryKind::Rollback,
            // A canary rollback means a shipped regression was actually
            // caught by the fleet health-gate — always High.
            severity: Severity::High,
            ts: rb.ts,
            summary: format!(
                "canary rolled {} back {}->{} at stage {} (reason={})",
                rb.plugin,
                rb.to_version,
                from,
                rb.stage,
                rb.reason.token()
            ),
            identifier: rb.plugin.clone(),
        });
    }

    for f in &dedup_findings(findings) {
        let severity = f
            .severity
            .as_deref()
            .map(normalize_severity)
            .unwrap_or(Severity::Medium);
        let sev = f
            .severity
            .as_deref()
            .map(|s| format!("[{s}] "))
            .unwrap_or_default();
        rows.push(ReviewQueueEntry {
            kind: EntryKind::AiFinding,
            severity,
            ts: f.ts,
            summary: format!("{}{} ({})", sev, f.summary, f.source),
            identifier: f.finding_id.clone(),
        });
    }

    // Risk-first: highest severity leads, then newest-first within a
    // severity band, with a deterministic tiebreak so equal
    // (severity, timestamp) pairs don't reorder between runs. This replaces
    // pure recency ordering — a stale High-severity item must never sink
    // below a fresh Low-severity one (see `stale_high_severity_outranks_
    // fresh_low_severity` for the regression guard).
    rows.sort_by(|a, b| {
        b.severity
            .cmp(&a.severity)
            .then_with(|| b.ts.cmp(&a.ts))
            .then_with(|| a.kind.tag().cmp(b.kind.tag()))
            .then_with(|| a.identifier.cmp(&b.identifier))
    });
    rows
}

/// Read all three sources (fail-soft), merge, and render the unified queue.
///
/// `since` filters to entries with `ts >= since` when supplied; `limit` caps
/// the number of rows shown (after ordering), keeping the top-K RISKIEST rows
/// since [`build_queue`] now sorts severity-first. In non-JSON mode, if rows
/// were shed by `limit` a trailing line reports how many lower-risk items
/// were deferred so nothing is silently lost.
pub fn run(json: bool, since: Option<i64>, limit: Option<usize>) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let now = store::now();

    // Source 1: systemic violations (reuse the item-B recurrence path).
    let events = store::read_violations(&cwd).unwrap_or_default();
    let systemic: Vec<SignatureRecurrence> =
        violation::detect_recurrence(&events, now, RecurrencePolicy::default())
            .into_iter()
            .filter(|r| r.is_systemic)
            .collect();

    // Source 2: canary rollback events.
    let rollbacks = store::read_rollbacks(&cwd).unwrap_or_default();

    // Source 3: AI-review findings (normally empty — no producer wired yet).
    let findings = store::read_review_findings(&cwd).unwrap_or_default();

    let mut rows = build_queue(&systemic, &rollbacks, &findings);

    if let Some(since_ts) = since {
        rows.retain(|r| r.ts >= since_ts);
    }
    // After the severity-first sort, `--limit` keeps the top-K RISKIEST rows
    // (not the freshest). Track how many lower-risk rows were shed so a
    // human running non-JSON mode knows items were dropped rather than
    // silently lost.
    let shed = limit.map(|n| rows.len().saturating_sub(n)).unwrap_or(0);
    if let Some(n) = limit {
        rows.truncate(n);
    }

    if json {
        // Keep JSON output as the bare array — no shed-count noise injected.
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }

    if rows.is_empty() {
        println!("(review queue empty — no systemic violations, rollbacks, or findings)");
        return Ok(());
    }
    for r in &rows {
        println!(
            "[{}] ts={} {}  <{}>",
            r.kind.tag(),
            r.ts,
            r.summary,
            r.identifier
        );
    }
    if shed > 0 {
        println!("({shed} lower-risk item(s) below the cut deferred)");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rollback::RollbackReason;
    use crate::violation::ViolationSource;

    fn sig(signature: &str, last_seen: i64) -> SignatureRecurrence {
        SignatureRecurrence {
            signature: signature.to_string(),
            source: ViolationSource::Blastguard,
            occurrences: 3,
            distinct_tasks: 3,
            distinct_sessions: 2,
            first_seen: last_seen - 10,
            last_seen,
            is_systemic: true,
        }
    }

    fn rb(plugin: &str, ts: i64) -> RollbackEvent {
        RollbackEvent::new(
            plugin.to_string(),
            Some("0.1.0".to_string()),
            "0.2.0".to_string(),
            0,
            RollbackReason::Raw,
            ts,
            None,
        )
    }

    fn finding(id: &str, ts: i64) -> ReviewFinding {
        ReviewFinding::new(
            id.to_string(),
            "reviewgate".to_string(),
            Some("high".to_string()),
            "a finding".to_string(),
            Some("src/x.rs".to_string()),
            ts,
        )
    }

    #[test]
    fn build_queue_merges_all_three_kinds_newest_first() {
        let systemic = vec![sig("blastguard:rm-rf", 100)];
        let rollbacks = vec![rb("overwatch", 300)];
        let findings = vec![finding("F-1", 200)];

        let q = build_queue(&systemic, &rollbacks, &findings);
        assert_eq!(q.len(), 3);
        // Newest-first: 300 (rollback), 200 (ai-finding), 100 (systemic).
        assert_eq!(q[0].kind, EntryKind::Rollback);
        assert_eq!(q[0].ts, 300);
        assert_eq!(q[1].kind, EntryKind::AiFinding);
        assert_eq!(q[1].ts, 200);
        assert_eq!(q[2].kind, EntryKind::Systemic);
        assert_eq!(q[2].ts, 100);
    }

    #[test]
    fn build_queue_missing_sources_degrade_gracefully() {
        // Only rollbacks present (systemic + findings empty): must still return
        // the rollback rows, not error / not drop everything.
        let q = build_queue(&[], &[rb("p", 10)], &[]);
        assert_eq!(q.len(), 1);
        assert_eq!(q[0].kind, EntryKind::Rollback);

        // All empty -> empty queue.
        assert!(build_queue(&[], &[], &[]).is_empty());
    }

    #[test]
    fn build_queue_tiebreak_is_deterministic() {
        // Same timestamp on all three: order must be stable and identical
        // across calls (kind tag then identifier).
        let s = vec![sig("blastguard:x", 50)];
        let r = vec![rb("p", 50)];
        let f = vec![finding("F", 50)];
        let q1 = build_queue(&s, &r, &f);
        let q2 = build_queue(&s, &r, &f);
        assert_eq!(q1, q2);
        // tags sorted: "ai-finding" < "rollback" < "systemic"
        assert_eq!(q1[0].kind, EntryKind::AiFinding);
        assert_eq!(q1[1].kind, EntryKind::Rollback);
        assert_eq!(q1[2].kind, EntryKind::Systemic);
    }

    #[test]
    fn entry_kind_serializes_kebab_case() {
        let j = serde_json::to_string(&EntryKind::AiFinding).unwrap();
        assert_eq!(j, "\"ai-finding\"");
    }

    #[test]
    fn review_queue_entry_carries_kind_discriminator_in_json() {
        let q = build_queue(&[], &[rb("overwatch", 1)], &[]);
        let json = serde_json::to_string(&q).unwrap();
        assert!(json.contains("\"kind\":\"rollback\""));
    }

    // --- finding-id dedup (Continuous-Audit re-record collapse) -------------

    /// A finding re-recorded across audit rounds (same `finding_id`) must
    /// collapse to ONE row carrying its newest `ts`, regardless of input order.
    #[test]
    fn build_queue_dedups_findings_by_id_keeping_latest_ts() {
        let old = finding("F-1", 100);
        let new = finding("F-1", 200);

        // Newest last, and newest first — both orders must yield one row @ 200.
        for findings in [
            vec![old.clone(), new.clone()],
            vec![new.clone(), old.clone()],
        ] {
            let q = build_queue(&[], &[], &findings);
            let ai: Vec<_> = q
                .iter()
                .filter(|r| r.kind == EntryKind::AiFinding)
                .collect();
            assert_eq!(ai.len(), 1, "same finding_id must collapse to one row");
            assert_eq!(ai[0].ts, 200, "the surfaced row must carry the newest ts");
            assert_eq!(ai[0].identifier, "F-1");
        }
    }

    /// Distinct finding ids are NOT collapsed — each keeps its own row.
    #[test]
    fn build_queue_keeps_distinct_finding_ids() {
        let findings = vec![
            finding("F-1", 100),
            finding("F-2", 100),
            finding("F-3", 100),
        ];
        let q = build_queue(&[], &[], &findings);
        let ai = q.iter().filter(|r| r.kind == EntryKind::AiFinding).count();
        assert_eq!(ai, 3, "distinct ids must not be deduped");
    }

    // --- severity-first risk ranking (da04890b) ------------------------------

    #[test]
    fn normalize_severity_parses_case_insensitively() {
        assert_eq!(normalize_severity("HIGH"), Severity::High);
        assert_eq!(normalize_severity("high"), Severity::High);
        assert_eq!(normalize_severity("High"), Severity::High);
        assert_eq!(normalize_severity("med"), Severity::Medium);
        assert_eq!(normalize_severity("MEDIUM"), Severity::Medium);
        assert_eq!(normalize_severity("low"), Severity::Low);
        assert_eq!(normalize_severity("LOW"), Severity::Low);
    }

    #[test]
    fn normalize_severity_defaults_unrecognized_to_medium() {
        // Unknown/garbage text must not silently sink to Low (that would bury
        // it); Medium is the documented default.
        assert_eq!(normalize_severity("garbage"), Severity::Medium);
        assert_eq!(normalize_severity(""), Severity::Medium);
    }

    /// The keystone regression guard: a STALE High-severity finding must sort
    /// BEFORE a FRESH Low-severity finding. Pure recency ordering (the old
    /// behavior) would put the fresh-low row first and, under `--limit`,
    /// could evict the stale-high row entirely.
    #[test]
    fn stale_high_severity_outranks_fresh_low_severity() {
        let stale_high = ReviewFinding::new(
            "F-STALE-HIGH".to_string(),
            "reviewgate".to_string(),
            Some("high".to_string()),
            "stale but dangerous".to_string(),
            None,
            100, // old ts
        );
        let fresh_low = ReviewFinding::new(
            "F-FRESH-LOW".to_string(),
            "reviewgate".to_string(),
            Some("low".to_string()),
            "fresh but minor".to_string(),
            None,
            999, // new ts
        );
        let q = build_queue(&[], &[], &[stale_high, fresh_low]);
        assert_eq!(q.len(), 2);
        assert_eq!(q[0].identifier, "F-STALE-HIGH", "stale-high must lead");
        assert_eq!(q[0].severity, Severity::High);
        assert_eq!(q[1].identifier, "F-FRESH-LOW");
        assert_eq!(q[1].severity, Severity::Low);
    }

    #[test]
    fn same_severity_falls_back_to_newest_first() {
        let old_high = finding("F-OLD", 100);
        let new_high = finding("F-NEW", 200);
        let q = build_queue(&[], &[], &[old_high, new_high]);
        assert_eq!(q[0].identifier, "F-NEW");
        assert_eq!(q[1].identifier, "F-OLD");
    }

    #[test]
    fn systemic_and_rollback_rows_default_to_high_severity() {
        let systemic = vec![sig("blastguard:x", 10)];
        let rollbacks = vec![rb("overwatch", 20)];
        let q = build_queue(&systemic, &rollbacks, &[]);
        for row in &q {
            assert_eq!(
                row.severity,
                Severity::High,
                "systemic/rollback rows are real already-happening problems"
            );
        }
    }

    #[test]
    fn top_k_after_sort_keeps_the_riskiest_rows() {
        // A stale-high plus several fresh-low findings: limiting to 1 must
        // keep the stale-high row, not the freshest low one.
        let stale_high = finding("F-HIGH", 10);
        let mut findings = vec![ReviewFinding::new(
            stale_high.finding_id.clone(),
            stale_high.source.clone(),
            Some("high".to_string()),
            stale_high.summary.clone(),
            None,
            10,
        )];
        for i in 0..5 {
            findings.push(ReviewFinding::new(
                format!("F-LOW-{i}"),
                "reviewgate".to_string(),
                Some("low".to_string()),
                "minor".to_string(),
                None,
                1000 + i,
            ));
        }
        let mut q = build_queue(&[], &[], &findings);
        q.truncate(1);
        assert_eq!(q[0].identifier, "F-HIGH");
    }

    /// Deduping the AI-findings stream must not touch the systemic/rollback
    /// streams: those rows are unaffected in count and identity.
    #[test]
    fn dedup_does_not_disturb_other_streams() {
        let systemic = vec![sig("blastguard:x", 10)];
        let rollbacks = vec![rb("overwatch", 20)];
        // Two records of the SAME finding id (collapse to 1) alongside the other
        // two streams (which must each still contribute exactly one row).
        let findings = vec![finding("F-1", 30), finding("F-1", 40)];
        let q = build_queue(&systemic, &rollbacks, &findings);
        assert_eq!(
            q.iter().filter(|r| r.kind == EntryKind::Systemic).count(),
            1
        );
        assert_eq!(
            q.iter().filter(|r| r.kind == EntryKind::Rollback).count(),
            1
        );
        assert_eq!(
            q.iter().filter(|r| r.kind == EntryKind::AiFinding).count(),
            1
        );
        assert_eq!(q.len(), 3);
    }
}
