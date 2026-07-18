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
use crate::merge_conflict::MergeConflictEntry;
use crate::review_escalation::{self, ConduktEscalation};
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
    /// An open condukt durable escalation (a blocked/GATED task awaiting an
    /// out-of-band human answer) — bridged in from condukt's
    /// `escalations.json`, foreign-read by path (see `review_escalation.rs`).
    Escalation,
    /// A blocked merge awaiting consensus resolution (design 625aa170 B): a
    /// real git 3-way conflict OR a gated mid-flight actual-diff overlap
    /// (decision A), recorded in `merge_conflicts.jsonl`, still unresolved.
    MergeConflict,
}

impl EntryKind {
    /// Short human tag shown in the default (non-JSON) output.
    pub fn tag(self) -> &'static str {
        match self {
            EntryKind::Systemic => "systemic",
            EntryKind::Rollback => "rollback",
            EntryKind::AiFinding => "ai-finding",
            EntryKind::Escalation => "escalation",
            EntryKind::MergeConflict => "merge-conflict",
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
    /// How many raw source records collapsed into this row (noise-collapse:
    /// repeated AI findings sharing a content fingerprint, or repeated
    /// same-plugin rollback events). `1` when no collapsing happened.
    /// Additive field: `#[serde(default)]` keeps old JSONL/fixtures reading
    /// as `1` (systemic rows, and any row predating this field).
    #[serde(default = "default_occurrences")]
    pub occurrences: u32,
}

/// Default for [`ReviewQueueEntry::occurrences`] on deserialize — back-compat
/// for rows recorded before the field existed (and for streams, like
/// systemic, that never collapse).
fn default_occurrences() -> u32 {
    1
}

/// Normalize a piece of finding text for fingerprint comparison: trim
/// leading/trailing whitespace, lowercase (ASCII), and collapse internal
/// whitespace runs to a single space. Pure and total (any `&str` in, a
/// normalized `String` out) so two records that only differ by incidental
/// formatting (casing, extra spaces) still fingerprint-match.
fn normalize_for_fingerprint(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

/// Content fingerprint for a [`ReviewFinding`]: `source` plus the normalized
/// `file` (empty string when absent) and normalized `summary`, joined with a
/// unit-separator (`\u{1f}`) so no legitimate field value can forge a
/// collision by embedding the delimiter. There is no rule-id field on
/// `ReviewFinding` — this is built only from real, existing fields.
pub(crate) fn finding_fingerprint(f: &ReviewFinding) -> String {
    let file = normalize_for_fingerprint(f.file.as_deref().unwrap_or(""));
    let summary = normalize_for_fingerprint(&f.summary);
    format!("{}\u{1f}{}\u{1f}{}", f.source, file, summary)
}

/// Deduplicate AI findings, collapsing two kinds of duplication:
///
/// 1. **Same `finding_id`** — the Continuous-Audit loop re-records a still-
///    confirmed finding every round with the same id (summary/severity may be
///    revised between rounds); this is the original identity rule.
/// 2. **Same content fingerprint** ([`finding_fingerprint`]: `source` +
///    normalized `file` + normalized `summary`) — independent reports of the
///    *same underlying issue* that happen to carry different `finding_id`s
///    (e.g. two audit passes minting fresh ids for the same finding).
///
/// A record can join a group via *either* rule (id-match OR fingerprint-match,
/// transitively) — implemented as a small union-find over the input indices so
/// chained matches merge correctly. Each resulting group collapses to ONE
/// representative record — the **newest** (`ts`); on an exact `ts` tie the
/// later record in `findings` wins ("last write wins" at the same instant),
/// deterministic because the input slice order is stable (append order of
/// `review_findings.jsonl`) and iteration never depends on hash order — plus
/// the group's **occurrence count** (how many raw records collapsed into it).
/// Only the AI-findings stream is touched — the systemic and rollback streams
/// never pass through here (rollbacks have their own same-plugin collapse in
/// [`build_queue`]).
pub(crate) fn dedup_findings(findings: &[ReviewFinding]) -> Vec<(ReviewFinding, u32)> {
    let n = findings.len();
    if n == 0 {
        return Vec::new();
    }

    // Union-find over indices 0..n, unioning i,j when they share either
    // identity rule above.
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(parent: &mut [usize], x: usize) -> usize {
        if parent[x] != x {
            parent[x] = find(parent, parent[x]);
        }
        parent[x]
    }
    fn union(parent: &mut [usize], a: usize, b: usize) {
        let ra = find(parent, a);
        let rb = find(parent, b);
        if ra != rb {
            // Deterministic: always attach the higher root to the lower one.
            if ra < rb {
                parent[rb] = ra;
            } else {
                parent[ra] = rb;
            }
        }
    }

    let fingerprints: Vec<String> = findings.iter().map(finding_fingerprint).collect();
    for i in 0..n {
        for j in (i + 1)..n {
            if findings[i].finding_id == findings[j].finding_id
                || fingerprints[i] == fingerprints[j]
            {
                union(&mut parent, i, j);
            }
        }
    }

    // Group indices by root. A BTreeMap keyed on the (deterministic) root
    // index avoids any hash-order dependence; within each group, indices are
    // pushed in ascending (i.e. input) order.
    use std::collections::BTreeMap;
    let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for i in 0..n {
        let root = find(&mut parent, i);
        groups.entry(root).or_default().push(i);
    }

    let mut out = Vec::with_capacity(groups.len());
    for (_, idxs) in groups {
        // Representative: newest ts; on a tie, the LATER index (later in the
        // original input order) wins — mirrors the "last write wins" rule.
        let mut best = idxs[0];
        for &idx in &idxs[1..] {
            if findings[idx].ts >= findings[best].ts {
                best = idx;
            }
        }
        out.push((findings[best].clone(), idxs.len() as u32));
    }
    out
}

/// Collapse repeated same-`plugin` [`RollbackEvent`]s into one representative
/// row per plugin, mirroring [`dedup_findings`]'s noise-collapse for the
/// rollback stream: a flapping canary that rolls the same plugin back
/// repeatedly should surface as ONE row (with an occurrence count), not one
/// row per event. Representative = newest `ts`; on a tie the later event in
/// `rollbacks` wins (same determinism rule as the finding dedup). Grouped via
/// a `BTreeMap<&str, _>` keyed on `plugin` so iteration never depends on hash
/// order.
fn collapse_rollbacks(rollbacks: &[RollbackEvent]) -> Vec<(RollbackEvent, u32)> {
    use std::collections::BTreeMap;
    let mut groups: BTreeMap<&str, Vec<&RollbackEvent>> = BTreeMap::new();
    for rb in rollbacks {
        groups.entry(rb.plugin.as_str()).or_default().push(rb);
    }
    let mut out = Vec::with_capacity(groups.len());
    for (_, events) in groups {
        let mut best = events[0];
        for &e in &events[1..] {
            if e.ts >= best.ts {
                best = e;
            }
        }
        out.push((best.clone(), events.len() as u32));
    }
    out
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
///   repeated same-plugin events are first collapsed to one row (newest
///   representative + an `occurrences` count) by [`collapse_rollbacks`] so a
///   flapping canary doesn't flood the queue;
/// * AI findings are timestamped at their `ts` and ranked via
///   [`normalize_severity`] on their free-text `severity` field
///   (missing/unrecognized → `Severity::Medium`); findings sharing either a
///   `finding_id` or a content fingerprint (source + normalized file +
///   normalized summary) are first collapsed to their newest record — plus an
///   `occurrences` count — by [`dedup_findings`], so a finding recurring
///   across audit rounds, or reported independently under a different id, is
///   ONE row.
///
/// * condukt durable escalations (a task blocked awaiting an out-of-band
///   human answer, bridged in fail-soft by path from condukt's
///   `escalations.json` — see `review_escalation.rs`) are timestamped at
///   `created_at`, keyed by their `id`, and always ranked `Severity::High` (a
///   blocked/GATED task stalled on a human answer is a real, already-
///   happening work stoppage — same rationale as the systemic and rollback
///   streams). condukt's own `add_escalation` already dedups identical open
///   asks upstream (content backpressure), so no further collapse happens
///   here (`occurrences` is always `1`).
///
/// A missing/empty source simply contributes no rows. Ties on
/// `(severity, ts)` are broken deterministically by (kind, identifier) so the
/// ordering is stable and reproducible.
pub fn build_queue(
    systemic: &[SignatureRecurrence],
    rollbacks: &[RollbackEvent],
    findings: &[ReviewFinding],
    escalations: &[ConduktEscalation],
    merge_conflicts: &[MergeConflictEntry],
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
            // Already a pre-aggregated recurrence count via `r.occurrences`
            // (folded upstream by `detect_recurrence`); this stream never
            // collapses further here.
            occurrences: 1,
        });
    }

    for (rb, occurrences) in collapse_rollbacks(rollbacks) {
        let from = rb.from_version.as_deref().unwrap_or("(new)");
        let mut summary = format!(
            "canary rolled {} back {}->{} at stage {} (reason={})",
            rb.plugin,
            rb.to_version,
            from,
            rb.stage,
            rb.reason.token()
        );
        if occurrences > 1 {
            summary.push_str(&format!(" ({occurrences}x)"));
        }
        rows.push(ReviewQueueEntry {
            kind: EntryKind::Rollback,
            // A canary rollback means a shipped regression was actually
            // caught by the fleet health-gate — always High.
            severity: Severity::High,
            ts: rb.ts,
            summary,
            identifier: rb.plugin.clone(),
            occurrences,
        });
    }

    for (f, occurrences) in dedup_findings(findings) {
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
        let mut summary = format!("{}{} ({})", sev, f.summary, f.source);
        if occurrences > 1 {
            summary.push_str(&format!(" ({occurrences}x)"));
        }
        rows.push(ReviewQueueEntry {
            kind: EntryKind::AiFinding,
            severity,
            ts: f.ts,
            summary,
            identifier: f.finding_id.clone(),
            occurrences,
        });
    }

    for mc in merge_conflicts {
        let files = if mc.conflicted_files.is_empty() {
            "(unknown files)".to_string()
        } else {
            mc.conflicted_files.join(", ")
        };
        rows.push(ReviewQueueEntry {
            kind: EntryKind::MergeConflict,
            // A blocked merge (real conflict or a gated mid-flight overlap) is a
            // real, already-happening work stoppage awaiting resolution — High.
            severity: Severity::High,
            ts: mc.ts,
            summary: format!(
                "[{}] merge of {} into {} held: {} file(s) [{}]",
                mc.origin.token(),
                mc.branch,
                mc.default_branch,
                mc.conflicted_files.len(),
                files
            ),
            identifier: mc.conflict_id.clone(),
            // Idempotent per conflict_id upstream (append_merge_conflict); no
            // further collapse here.
            occurrences: 1,
        });
    }

    for e in escalations {
        rows.push(ReviewQueueEntry {
            kind: EntryKind::Escalation,
            // A blocked/GATED task stalled on a human answer is a real,
            // already-happening work stoppage — always High.
            severity: Severity::High,
            ts: e.created_at,
            summary: format!(
                "awaiting human answer: {} (run {} task {})",
                e.question, e.run, e.task
            ),
            identifier: e.id.clone(),
            // condukt's add_escalation already dedups identical OPEN asks
            // upstream (content backpressure); no further collapse here.
            occurrences: 1,
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

    // Source 4: condukt's durable escalation queue, foreign-read by path
    // (fail-soft: absent condukt / no open escalations contributes nothing).
    let escalations = review_escalation::read_open_escalations(&cwd);

    // Source 5: OPEN blocked merges (real conflicts + gated mid-flight overlaps),
    // fail-soft (absent/empty store contributes nothing).
    let merge_conflicts = store::open_merge_conflicts(&cwd).unwrap_or_default();

    let mut rows = build_queue(
        &systemic,
        &rollbacks,
        &findings,
        &escalations,
        &merge_conflicts,
    );

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
        println!(
            "(review queue empty — no systemic violations, rollbacks, findings, escalations, or merge conflicts)"
        );
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
            None,
            ts,
        )
    }

    /// Like [`finding`] but with an explicit, distinct `summary` — used where
    /// a test needs findings that must NOT fingerprint-collide (unlike
    /// `finding`, which fixes the same summary/file/source for every call).
    fn finding_with(id: &str, summary: &str, ts: i64) -> ReviewFinding {
        ReviewFinding::new(
            id.to_string(),
            "reviewgate".to_string(),
            Some("high".to_string()),
            summary.to_string(),
            Some("src/x.rs".to_string()),
            None,
            ts,
        )
    }

    fn esc(id: &str, run: &str, task: &str, question: &str, ts: i64) -> ConduktEscalation {
        ConduktEscalation {
            id: id.to_string(),
            run: run.to_string(),
            task: task.to_string(),
            question: question.to_string(),
            resolved: false,
            created_at: ts,
        }
    }

    /// Pin the exact bytes of [`finding_fingerprint`] so any future change to
    /// the normalization (whitespace collapse, trim, ascii-lowercase) or the
    /// unit-separator join is caught. This is the shared key both the dedup
    /// grouping and the to-backlog idempotency contract (CA-overwatch-01) rely
    /// on; drifting it silently would break already-bridged recognition.
    #[test]
    fn finding_fingerprint_bytes_are_pinned() {
        let sample = ReviewFinding::new(
            "f-1".to_string(),
            "reviewgate".to_string(),
            Some("high".to_string()),
            // mixed case + collapsible internal whitespace (spaces + a tab)
            "  Duplicate\tHelper   Across   Modules  ".to_string(),
            // irregular surrounding whitespace + mixed case
            Some("  Crates/Foo.rs  ".to_string()),
            None,
            42,
        );
        // source + US + normalized(file) + US + normalized(summary), where
        // normalize = collapse whitespace to single spaces, trim, ascii-lowercase.
        let expected = "reviewgate\u{1f}crates/foo.rs\u{1f}duplicate helper across modules";
        assert_eq!(finding_fingerprint(&sample), expected);
    }

    #[test]
    fn build_queue_merges_all_three_kinds_newest_first() {
        let systemic = vec![sig("blastguard:rm-rf", 100)];
        let rollbacks = vec![rb("overwatch", 300)];
        let findings = vec![finding("F-1", 200)];

        let q = build_queue(&systemic, &rollbacks, &findings, &[], &[]);
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
        let q = build_queue(&[], &[rb("p", 10)], &[], &[], &[]);
        assert_eq!(q.len(), 1);
        assert_eq!(q[0].kind, EntryKind::Rollback);

        // All empty -> empty queue.
        assert!(build_queue(&[], &[], &[], &[], &[]).is_empty());
    }

    #[test]
    fn build_queue_tiebreak_is_deterministic() {
        // Same timestamp on all three: order must be stable and identical
        // across calls (kind tag then identifier).
        let s = vec![sig("blastguard:x", 50)];
        let r = vec![rb("p", 50)];
        let f = vec![finding("F", 50)];
        let q1 = build_queue(&s, &r, &f, &[], &[]);
        let q2 = build_queue(&s, &r, &f, &[], &[]);
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
        let q = build_queue(&[], &[rb("overwatch", 1)], &[], &[], &[]);
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
            let q = build_queue(&[], &[], &findings, &[], &[]);
            let ai: Vec<_> = q
                .iter()
                .filter(|r| r.kind == EntryKind::AiFinding)
                .collect();
            assert_eq!(ai.len(), 1, "same finding_id must collapse to one row");
            assert_eq!(ai[0].ts, 200, "the surfaced row must carry the newest ts");
            assert_eq!(ai[0].identifier, "F-1");
        }
    }

    /// Distinct finding ids with DISTINCT content are NOT collapsed — each
    /// keeps its own row (they neither share an id nor a content fingerprint).
    #[test]
    fn build_queue_keeps_distinct_finding_ids() {
        let findings = vec![
            finding_with("F-1", "finding one", 100),
            finding_with("F-2", "finding two", 100),
            finding_with("F-3", "finding three", 100),
        ];
        let q = build_queue(&[], &[], &findings, &[], &[]);
        let ai = q.iter().filter(|r| r.kind == EntryKind::AiFinding).count();
        assert_eq!(
            ai, 3,
            "distinct ids with distinct content must not be deduped"
        );
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
            None,
            100, // old ts
        );
        let fresh_low = ReviewFinding::new(
            "F-FRESH-LOW".to_string(),
            "reviewgate".to_string(),
            Some("low".to_string()),
            "fresh but minor".to_string(),
            None,
            None,
            999, // new ts
        );
        let q = build_queue(&[], &[], &[stale_high, fresh_low], &[], &[]);
        assert_eq!(q.len(), 2);
        assert_eq!(q[0].identifier, "F-STALE-HIGH", "stale-high must lead");
        assert_eq!(q[0].severity, Severity::High);
        assert_eq!(q[1].identifier, "F-FRESH-LOW");
        assert_eq!(q[1].severity, Severity::Low);
    }

    #[test]
    fn same_severity_falls_back_to_newest_first() {
        // Distinct content (not just distinct ids) so they don't
        // fingerprint-collapse into one row — this test is about severity/ts
        // tiebreak ordering between two genuinely distinct findings.
        let old_high = finding_with("F-OLD", "old finding content", 100);
        let new_high = finding_with("F-NEW", "new finding content", 200);
        let q = build_queue(&[], &[], &[old_high, new_high], &[], &[]);
        assert_eq!(q[0].identifier, "F-NEW");
        assert_eq!(q[1].identifier, "F-OLD");
    }

    #[test]
    fn systemic_and_rollback_rows_default_to_high_severity() {
        let systemic = vec![sig("blastguard:x", 10)];
        let rollbacks = vec![rb("overwatch", 20)];
        let q = build_queue(&systemic, &rollbacks, &[], &[], &[]);
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
                None,
                1000 + i,
            ));
        }
        let mut q = build_queue(&[], &[], &findings, &[], &[]);
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
        let q = build_queue(&systemic, &rollbacks, &findings, &[], &[]);
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

    // --- fingerprint dedup + rollback collapse (occurrences) -----------------

    /// Two findings with DIFFERENT `finding_id`s but the SAME
    /// (source, file, summary) content must collapse to ONE row via the
    /// content fingerprint, carrying `occurrences == 2`. This is the case the
    /// old exact-`finding_id` dedup could not catch (RED on pre-change code:
    /// it would keep both as separate rows).
    #[test]
    fn build_queue_collapses_same_fingerprint_across_distinct_ids() {
        let a = ReviewFinding::new(
            "F-A".to_string(),
            "reviewgate".to_string(),
            Some("high".to_string()),
            "unchecked unwrap on user input".to_string(),
            Some("src/foo.rs".to_string()),
            None,
            100,
        );
        let b = ReviewFinding::new(
            "F-B".to_string(),
            "reviewgate".to_string(),
            Some("high".to_string()),
            "  Unchecked  UNWRAP on user input ".to_string(), // same after normalize
            Some("src/foo.rs".to_string()),
            None,
            200,
        );
        let q = build_queue(&[], &[], &[a, b], &[], &[]);
        let ai: Vec<_> = q
            .iter()
            .filter(|r| r.kind == EntryKind::AiFinding)
            .collect();
        assert_eq!(
            ai.len(),
            1,
            "same content fingerprint under different ids must collapse to one row"
        );
        assert_eq!(ai[0].occurrences, 2);
        assert_eq!(
            ai[0].ts, 200,
            "the newest record must be the representative"
        );
        assert!(
            ai[0].summary.contains("(2x)"),
            "collapsed summary must surface the occurrence count: {}",
            ai[0].summary
        );
    }

    /// Two `RollbackEvent`s for the SAME plugin must collapse to ONE
    /// `Rollback` row carrying `occurrences == 2`.
    #[test]
    fn build_queue_collapses_repeated_same_plugin_rollbacks() {
        let rollbacks = vec![rb("overwatch", 10), rb("overwatch", 20)];
        let q = build_queue(&[], &rollbacks, &[], &[], &[]);
        let rb_rows: Vec<_> = q.iter().filter(|r| r.kind == EntryKind::Rollback).collect();
        assert_eq!(
            rb_rows.len(),
            1,
            "repeated same-plugin rollbacks must collapse to one row"
        );
        assert_eq!(rb_rows[0].occurrences, 2);
        assert_eq!(
            rb_rows[0].ts, 20,
            "the newest event must be the representative"
        );
        assert!(
            rb_rows[0].summary.contains("(2x)"),
            "collapsed summary must surface the occurrence count: {}",
            rb_rows[0].summary
        );
    }

    /// Distinct-plugin rollbacks must NOT collapse into each other.
    #[test]
    fn build_queue_keeps_distinct_plugin_rollbacks_separate() {
        let rollbacks = vec![rb("overwatch", 10), rb("condukt", 20)];
        let q = build_queue(&[], &rollbacks, &[], &[], &[]);
        let rb_rows = q.iter().filter(|r| r.kind == EntryKind::Rollback).count();
        assert_eq!(rb_rows, 2, "distinct plugins must not be collapsed");
    }

    /// A row with no collapsing (fresh row, no duplicates) must default to
    /// `occurrences == 1` and carry no `(Nx)` marker.
    #[test]
    fn occurrences_defaults_to_one_when_no_collapse_happened() {
        let systemic = vec![sig("blastguard:x", 10)];
        let rollbacks = vec![rb("overwatch", 20)];
        let findings = vec![finding("F-1", 30)];
        let q = build_queue(&systemic, &rollbacks, &findings, &[], &[]);
        for row in &q {
            assert_eq!(
                row.occurrences, 1,
                "no-collapse rows must read occurrences=1"
            );
            assert!(!row.summary.contains("(1x)"), "no marker for a lone record");
        }
    }

    // --- condukt escalation bridge (EntryKind::Escalation) -------------------

    /// RED->GREEN feature proof: a single OPEN condukt escalation must surface
    /// as a High-severity `Escalation` row with the right ts/identifier, sorted
    /// among the other High-severity rows per the usual (severity, ts, kind,
    /// identifier) rule. This test fails to compile before `EntryKind::
    /// Escalation` and `build_queue`'s 4th param exist, and passes after.
    #[test]
    fn build_queue_surfaces_one_open_escalation_as_high_severity_row() {
        let systemic = vec![sig("blastguard:rm-rf", 100)];
        let rollbacks = vec![rb("overwatch", 300)];
        let escalations = vec![esc("esc-1", "runA", "t1", "Which approach?", 200)];

        let q = build_queue(&systemic, &rollbacks, &[], &escalations, &[]);
        assert_eq!(q.len(), 3);

        let escalation_row = q
            .iter()
            .find(|r| r.kind == EntryKind::Escalation)
            .expect("an Escalation row must be present");
        assert_eq!(escalation_row.severity, Severity::High);
        assert_eq!(escalation_row.ts, 200);
        assert_eq!(escalation_row.identifier, "esc-1");
        assert_eq!(escalation_row.occurrences, 1);
        assert!(
            escalation_row.summary.contains("Which approach?")
                && escalation_row.summary.contains("runA")
                && escalation_row.summary.contains("t1"),
            "summary must name the question and its run/task context: {}",
            escalation_row.summary
        );

        // Sort position: all three rows are High severity, so ts DESC decides
        // (300 rollback, 200 escalation, 100 systemic).
        assert_eq!(q[0].kind, EntryKind::Rollback);
        assert_eq!(q[1].kind, EntryKind::Escalation);
        assert_eq!(q[2].kind, EntryKind::Systemic);
    }

    // --- merge-conflict kind (design 625aa170 B / decision A) ----------------

    fn mc(id: &str, origin: crate::merge_conflict::ConflictOrigin, ts: i64) -> MergeConflictEntry {
        MergeConflictEntry {
            conflict_id: id.to_string(),
            origin,
            run_id: "runA".to_string(),
            branch: "condukt/t2".to_string(),
            default_branch: "main".to_string(),
            base_ref: "base".to_string(),
            conflicted_files: vec!["crates/x/src/main.rs".to_string()],
            diff_ours: "ours-diff".to_string(),
            diff_theirs: "theirs-diff".to_string(),
            ts,
        }
    }

    /// RED->GREEN feature proof: an open merge conflict surfaces as a
    /// High-severity `[merge-conflict]` row naming the conflicted file, and a
    /// gated runtime-overlap surfaces under the SAME kind (unified surface).
    #[test]
    fn build_queue_surfaces_open_merge_conflict_as_high_severity_row() {
        use crate::merge_conflict::ConflictOrigin;
        let real = mc("c-real", ConflictOrigin::MergeConflict, 300);
        let overlap = mc("c-overlap", ConflictOrigin::RuntimeOverlap, 250);

        let q = build_queue(&[], &[], &[], &[], &[real, overlap]);
        assert_eq!(q.len(), 2);
        assert!(q.iter().all(|r| r.kind == EntryKind::MergeConflict));
        assert!(q.iter().all(|r| r.severity == Severity::High));
        // Newest-first within the High band: c-real (300) before c-overlap (250).
        assert_eq!(q[0].identifier, "c-real");
        assert!(q[0].summary.contains("crates/x/src/main.rs"));
        assert!(q[0].summary.contains("merge-conflict"));
        assert_eq!(q[1].identifier, "c-overlap");
        assert!(
            q[1].summary.contains("runtime-overlap"),
            "overlap origin must be marked: {}",
            q[1].summary
        );
    }

    #[test]
    fn merge_conflict_entry_kind_serializes_kebab_case() {
        let j = serde_json::to_string(&EntryKind::MergeConflict).unwrap();
        assert_eq!(j, "\"merge-conflict\"");
    }

    /// Backward-compat: an existing 3-source scenario with `escalations = &[]`
    /// must yield the exact same rows as before this source existed — an empty
    /// escalation slice contributes nothing.
    #[test]
    fn build_queue_empty_escalations_is_backward_compatible() {
        let systemic = vec![sig("blastguard:rm-rf", 100)];
        let rollbacks = vec![rb("overwatch", 300)];
        let findings = vec![finding("F-1", 200)];

        let with_empty_escalations = build_queue(&systemic, &rollbacks, &findings, &[], &[]);
        assert_eq!(with_empty_escalations.len(), 3);
        assert!(with_empty_escalations
            .iter()
            .all(|r| r.kind != EntryKind::Escalation));
        // Identical to the pre-existing three-kind merge test's expectations.
        assert_eq!(with_empty_escalations[0].kind, EntryKind::Rollback);
        assert_eq!(with_empty_escalations[0].ts, 300);
        assert_eq!(with_empty_escalations[1].kind, EntryKind::AiFinding);
        assert_eq!(with_empty_escalations[1].ts, 200);
        assert_eq!(with_empty_escalations[2].kind, EntryKind::Systemic);
        assert_eq!(with_empty_escalations[2].ts, 100);
    }
}
