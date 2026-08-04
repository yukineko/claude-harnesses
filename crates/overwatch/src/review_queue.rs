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
/// slices ([`build_queue`]); the CLI shell ([`run`]) reads the stores and
/// renders either a human-readable list or a JSON array.
///
/// # A source that was READ and held nothing vs one that could not be read
///
/// A missing source contributes nothing — that is a real observation of zero.
/// A source that could not be READ is a different answer, and this command
/// keeps the two apart on all three of its channels (t3):
///
/// * **stderr** — a `WARNING` naming the ledger and saying the source is
///   omitted;
/// * **stdout, human** — an `[undetermined-source]` row at the top of the list,
///   and the "queue empty" sentence is only ever printed when every source was
///   actually read (so it can never name a source it could not see as absent);
/// * **stdout, `--json`** — the SAME row, in band, in the same array
///   ([`EntryKind::UndeterminedSource`]), plus exit code 3.
///
/// The JSON shape is deliberately unchanged (still a bare array of rows, each
/// with a `kind`): the in-band row means `length == 0` can no longer be read as
/// "clean" by any consumer, without breaking the ones that already filter on
/// `kind`. Those kind-filtering consumers WOULD still skip the marker row, so
/// the exit code carries the same fact a second way — see [`SourceHealth`].
use crate::merge_conflict::MergeConflictEntry;
use crate::review_escalation::{self, ConduktEscalation};
use crate::review_finding::{AuditVerdict, ReviewFinding};
use crate::rollback::RollbackEvent;
use crate::store;
use crate::violation::{self, RecurrencePolicy, SignatureRecurrence};
use anyhow::Result;
use harness_core::verdict::Determination;
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
    /// NOT an item found in a source — the record that one of the five sources
    /// could not be read (or held a line that could not be decoded), so its
    /// items are missing from this queue.
    ///
    /// It rides IN BAND, in the same array as the real rows, because a queue
    /// rendered from an unreadable ledger is byte-identical to a clean one
    /// otherwise: a `--json` consumer checking `length == 0`, or a human
    /// reading "review queue empty", would take "I could not look" for "there
    /// is nothing there". Emitted only by [`run`] — never by [`build_queue`],
    /// so it can never be mistaken for a source record, and the backlog drain
    /// ([`crate::bridge`]) never sees one.
    UndeterminedSource,
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
            EntryKind::UndeterminedSource => "undetermined-source",
        }
    }
}

/// Whether every source behind a rendered/drained queue was actually READ.
///
/// Returned by the commands that consume the review queue so the CLI shell can
/// map "I could not read one of my sources" to a distinct exit code (3, the
/// same convention `canary-gate` uses for "this is an answer, not a crash"),
/// instead of the exit 0 that a shell reads as "ran fine, nothing to see".
/// There is deliberately no `Default` and no `From<bool>`: the value must be
/// derived from what the reads actually returned.
#[must_use = "the caller must map an incomplete queue to a non-zero exit"]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceHealth {
    /// Every source was read (some may legitimately have held nothing).
    AllRead,
    /// At least one source could not be read, or could not be filtered.
    SomeUndetermined,
}

impl SourceHealth {
    /// Process exit code: 0 when the answer is complete, 3 when it is not.
    pub fn exit_code(self) -> i32 {
        match self {
            SourceHealth::AllRead => 0,
            SourceHealth::SomeUndetermined => 3,
        }
    }
}

/// Static identity of one review-queue source, so the warning text, the in-band
/// marker row and the empty-queue prose all name it the same way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceMeta {
    /// The queue kind tag whose rows this source produces.
    pub tag: &'static str,
    /// Human label for the ledger, used in the warning.
    pub ledger: &'static str,
    /// The file name, used as the marker row's `identifier`.
    pub file: &'static str,
    /// Plural noun for the items this source holds ("canary rollbacks"), used
    /// by the empty-queue prose, which names only the sources it actually read.
    pub noun: &'static str,
}

/// Source 1: systemic gate violations.
pub const SRC_SYSTEMIC: SourceMeta = SourceMeta {
    tag: "systemic",
    ledger: "the violation ledger (violations.jsonl)",
    file: "violations.jsonl",
    noun: "systemic violations",
};
/// Source 2: canary rollback events.
pub const SRC_ROLLBACK: SourceMeta = SourceMeta {
    tag: "rollback",
    ledger: "the rollback ledger (rollbacks.jsonl)",
    file: "rollbacks.jsonl",
    noun: "rollbacks",
};
/// Source 3: AI/adversarial review findings.
pub const SRC_AI_FINDING: SourceMeta = SourceMeta {
    tag: "ai-finding",
    ledger: "the review-findings ledger (review_findings.jsonl)",
    file: "review_findings.jsonl",
    noun: "findings",
};
/// Source 4: condukt's durable escalation queue (foreign read by path).
pub const SRC_ESCALATION: SourceMeta = SourceMeta {
    tag: "escalation",
    ledger: "condukt's escalation queue (escalations.json)",
    file: "escalations.json",
    noun: "escalations",
};
/// Source 5: open blocked merges.
pub const SRC_MERGE_CONFLICT: SourceMeta = SourceMeta {
    tag: "merge-conflict",
    ledger: "the blocked-merge ledger (merge_conflicts.jsonl)",
    file: "merge_conflicts.jsonl",
    noun: "merge conflicts",
};
/// The join partner of source 5. Not a source of rows of its own: it only
/// FILTERS source 5 (resolved conflicts drop out).
pub const SRC_MERGE_RESOLUTION: SourceMeta = SourceMeta {
    tag: "merge-conflict",
    ledger: "the merge-conflict resolution ledger (merge_conflict_resolutions.jsonl)",
    file: "merge_conflict_resolutions.jsonl",
    noun: "merge conflicts",
};

/// What an undetermined read did to the queue. The two are NOT the same event
/// and must not be reported as one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceEffect {
    /// The source contributed NO rows: its items, if any, are missing here.
    Omitted,
    /// The source's rows ARE shown, but the ledger that filters them could not
    /// be read, so rows that no longer belong may be listed (over-reporting).
    ShownUnfiltered,
}

/// One source that could not be determined, carried from the reads to every
/// rendering channel so none of them has to re-derive the wording.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UndeterminedSource {
    /// Which source (and how it is named everywhere).
    pub meta: SourceMeta,
    /// What its being undetermined did to this queue.
    pub effect: SourceEffect,
    /// Why it could not be determined, forwarded from the reader.
    pub why: String,
}

impl UndeterminedSource {
    /// The stderr WARNING for this source. Says which ledger, what happened to
    /// the source, and — for the omitted case — that this is NOT a report of
    /// zero (the sentence source 1 and 3 have carried since they were migrated).
    pub fn warning(&self) -> String {
        match self.effect {
            SourceEffect::Omitted => format!(
                "overwatch review-queue: WARNING — {} could not be read or held an \
                 undecodable line ({}); the [{}] source is OMITTED from this queue. \
                 This is NOT a report of zero {}.",
                self.meta.ledger, self.why, self.meta.tag, self.meta.noun
            ),
            SourceEffect::ShownUnfiltered => format!(
                "overwatch review-queue: WARNING — {} could not be read or held an \
                 undecodable line ({}); every entry is therefore shown as UNRESOLVED. \
                 No [{}] row is hidden by this, but an already-resolved one may be \
                 listed.",
                self.meta.ledger, self.why, self.meta.tag
            ),
        }
    }

    /// The in-band marker row (see [`EntryKind::UndeterminedSource`]).
    /// `High` severity and `ts = now` so it leads the queue and is never shed
    /// by `--since`/`--limit`.
    pub fn row(&self, now: i64) -> ReviewQueueEntry {
        let summary = match self.effect {
            SourceEffect::Omitted => format!(
                "[{}] SOURCE NOT SHOWN — {} could not be read or held an undecodable \
                 line; its items are MISSING from this queue (this is not a report of \
                 zero): {}",
                self.meta.tag, self.meta.ledger, self.why
            ),
            SourceEffect::ShownUnfiltered => format!(
                "[{}] SOURCE NOT FILTERED — {} could not be read or held an undecodable \
                 line; every entry is shown as unresolved, so an already-resolved one \
                 may be listed: {}",
                self.meta.tag, self.meta.ledger, self.why
            ),
        };
        ReviewQueueEntry {
            kind: EntryKind::UndeterminedSource,
            severity: Severity::High,
            ts: now,
            summary,
            identifier: self.meta.file.to_string(),
            occurrences: 1,
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
        //
        // CA-overwatch-001: ts-only selection let a later same-fingerprint
        // Unverified/Refuted record SHADOW an earlier Confirmed one, and
        // `plan_finding_bridges` filters on the representative's verdict alone
        // — so the whole group silently vanished from the backlog bridge even
        // though Confirmed evidence exists in it. An actionable
        // (`is_actionable()`) record therefore always outranks a
        // non-actionable one regardless of `ts`; ties within the same
        // actionability still resolve by newest `ts` (unchanged rule).
        let mut best = idxs[0];
        for &idx in &idxs[1..] {
            let candidate_wins = match (
                findings[idx].verdict.is_actionable(),
                findings[best].verdict.is_actionable(),
            ) {
                (true, false) => true,
                (false, true) => false,
                _ => findings[idx].ts >= findings[best].ts,
            };
            if candidate_wins {
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
        // Non-CONFIRMED verdicts are marked inline so a row is never read as
        // an established finding. UNVERIFIED means "undetermined — still
        // pending re-verification", not "handled": it stays visible here but
        // is NOT bridged into the backlog as actionable work (see
        // `bridge::plan_finding_bridges`).
        let verdict_tag = match f.verdict {
            AuditVerdict::Confirmed => String::new(),
            other => format!("[{}] ", other.label().to_ascii_uppercase()),
        };
        let mut summary = format!("{}{}{} ({})", verdict_tag, sev, f.summary, f.source);
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

/// Resolve one scanned source for rendering: `Known(rows)` contributes its rows
/// (an empty vec is a real "read it, it held nothing"); `Undetermined(why)`
/// contributes NO rows and instead records an [`UndeterminedSource`] in `sink`,
/// which is what every rendering channel reads to say the source is missing.
///
/// Deliberately NOT a `unwrap_or_default()`: the whole point is that the empty
/// vec returned here is accompanied by a record saying it is not an observation
/// of zero.
fn resolve_source<T>(
    scan: Determination<Vec<T>>,
    meta: SourceMeta,
    sink: &mut Vec<UndeterminedSource>,
) -> Vec<T> {
    match scan {
        Determination::Known(rows) => rows,
        Determination::Undetermined(why) => {
            sink.push(UndeterminedSource {
                meta,
                effect: SourceEffect::Omitted,
                why: why.as_str().to_string(),
            });
            Vec::new()
        }
    }
}

/// The prose for a queue with no rows at all. It may only name the sources that
/// were ACTUALLY READ — hence the argument.
///
/// In practice `undetermined` is always empty when this is called, because an
/// undetermined source contributes a marker row and the row list is therefore
/// not empty (see [`assemble`], whose test pins that invariant). The parameter
/// exists so this function cannot be MADE to lie by a future caller: if any
/// source was undetermined it says so instead of listing it as absent.
fn empty_queue_line(undetermined: &[UndeterminedSource]) -> String {
    if undetermined.is_empty() {
        return "(review queue empty — no systemic violations, rollbacks, findings, escalations, or merge conflicts)"
            .to_string();
    }
    let read: Vec<&str> = [
        SRC_SYSTEMIC,
        SRC_ROLLBACK,
        SRC_AI_FINDING,
        SRC_ESCALATION,
        SRC_MERGE_CONFLICT,
    ]
    .iter()
    .filter(|m| !undetermined.iter().any(|u| u.meta.tag == m.tag))
    .map(|m| m.noun)
    .collect();
    if read.is_empty() {
        return "(NO source could be read — see the UNDETERMINED source(s) above; this is NOT \
                a report that the queue is empty)"
            .to_string();
    }
    format!(
        "(no {} — but see the UNDETERMINED source(s) above; this is NOT a report that the \
         queue is empty)",
        read.join(", ")
    )
}

/// Assemble the final row list: the real rows (already ordered, filtered and
/// capped) with one marker row per undetermined source PREPENDED.
///
/// Prepending after the cap is deliberate. A marker row is not a queue item
/// competing for the top-K: it is the statement that the top-K may be missing
/// items, so `--since` and `--limit` must not be able to shed it (that would
/// re-create the silent degrade this whole path exists to remove).
fn assemble(
    rows: Vec<ReviewQueueEntry>,
    undetermined: &[UndeterminedSource],
    now: i64,
) -> Vec<ReviewQueueEntry> {
    let mut out: Vec<ReviewQueueEntry> = undetermined.iter().map(|u| u.row(now)).collect();
    out.extend(rows);
    out
}

/// Read all five sources, merge, and render the unified queue.
///
/// `since` filters to entries with `ts >= since` when supplied; `limit` caps
/// the number of rows shown (after ordering), keeping the top-K RISKIEST rows
/// since [`build_queue`] now sorts severity-first. In non-JSON mode, if rows
/// were shed by `limit` a trailing line reports how many lower-risk items
/// were deferred so nothing is silently lost.
///
/// Returns the [`SourceHealth`] of the read so the CLI can exit non-zero when
/// the rendered queue is incomplete. This command renders and returns no
/// verdict about the CODE — but "I could not read a source" is a verdict about
/// the ANSWER, and swallowing it would leave the two states identical to a
/// script (CLAUDE.md §1: 沈黙は許容される degrade ではない).
pub fn run(json: bool, since: Option<i64>, limit: Option<usize>) -> Result<SourceHealth> {
    let cwd = std::env::current_dir()?;
    let now = store::now();
    let mut undetermined: Vec<UndeterminedSource> = Vec::new();

    // Source 1: systemic violations (reuse the item-B recurrence path).
    // `ViolationScan` is this module's own tri-state (it predates the shared
    // `Determination` and carries no reason payload), so it is matched here
    // rather than routed through `resolve_source`.
    let systemic: Vec<SignatureRecurrence> = match store::scan_violations(&cwd) {
        // Absent = no violation was ever recorded: a real, trustworthy empty.
        store::ViolationScan::Absent => Vec::new(),
        store::ViolationScan::Events(events) => {
            violation::detect_recurrence(&events, now, RecurrencePolicy::default())
                .into_iter()
                .filter(|r| r.is_systemic)
                .collect()
        }
        store::ViolationScan::Undetermined => {
            undetermined.push(UndeterminedSource {
                meta: SRC_SYSTEMIC,
                effect: SourceEffect::Omitted,
                // `ViolationScan::Undetermined` carries no reason (and mints no
                // `Determination`, so nothing is double-counted by naming it
                // here); the two causes it folds together are stated instead.
                why: "unreadable file, or a line that failed to parse".to_string(),
            });
            Vec::new()
        }
    };

    // Source 2: canary rollback events.
    let rollbacks = resolve_source(
        store::scan_rollbacks(&cwd)?,
        SRC_ROLLBACK,
        &mut undetermined,
    );

    // Source 3: AI-review findings. An Undetermined scan must NOT collapse to
    // empty: the ledger could hold a real, already-CONFIRMED adversarial
    // finding that simply failed to read back, and the review queue exists
    // precisely to surface that — never to silently drop it.
    let findings: Vec<ReviewFinding> = match store::scan_review_findings(&cwd) {
        // Absent = no finding was ever recorded: a real, trustworthy empty.
        store::ReviewFindingScan::Absent => Vec::new(),
        store::ReviewFindingScan::Findings(findings) => findings,
        store::ReviewFindingScan::Undetermined(reason) => {
            undetermined.push(UndeterminedSource {
                meta: SRC_AI_FINDING,
                effect: SourceEffect::Omitted,
                why: reason,
            });
            Vec::new()
        }
    };

    // Source 4: condukt's durable escalation queue, foreign-read by path. An
    // absent condukt (or no open ask) is a real zero; a queue that could not be
    // read is a HUMAN QUESTION we cannot see, and is announced.
    let escalations = resolve_source(
        review_escalation::scan_open_escalations(&cwd),
        SRC_ESCALATION,
        &mut undetermined,
    );

    // Source 5: OPEN blocked merges (real conflicts + gated mid-flight
    // overlaps). Two ledgers, failing in opposite directions — see
    // `store::scan_open_merge_conflicts`: an unreadable ENTRY ledger omits the
    // source, an unreadable RESOLUTION ledger shows every entry unfiltered.
    let merge_scan = store::scan_open_merge_conflicts(&cwd)?;
    if let Some(why) = merge_scan.resolutions_undetermined {
        undetermined.push(UndeterminedSource {
            meta: SRC_MERGE_RESOLUTION,
            effect: SourceEffect::ShownUnfiltered,
            why,
        });
    }
    let merge_conflicts = resolve_source(merge_scan.open, SRC_MERGE_CONFLICT, &mut undetermined);

    for u in &undetermined {
        eprintln!("{}", u.warning());
    }

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
    let rows = assemble(rows, &undetermined, now);

    let health = if undetermined.is_empty() {
        SourceHealth::AllRead
    } else {
        SourceHealth::SomeUndetermined
    };

    if json {
        // Still the bare array of rows: the undetermined sources are IN it, as
        // `kind: "undetermined-source"` rows, so `length == 0` cannot be read
        // as "clean" and consumers that filter on `kind` keep working. The
        // non-zero exit (SourceHealth) covers those filtering consumers, which
        // would otherwise drop the marker row on the floor.
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(health);
    }

    if rows.is_empty() {
        println!("{}", empty_queue_line(&undetermined));
        return Ok(health);
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
    Ok(health)
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

    /// An UNVERIFIED finding must still be VISIBLE on the review surface (it
    /// is not discarded), but must be labelled as such so it is never read as
    /// an established, confirmed finding.
    #[test]
    fn unverified_finding_row_is_labelled_unverified() {
        let f = finding("F-U", 200).with_verdict(AuditVerdict::Unverified);
        let rows = build_queue(&[], &[], std::slice::from_ref(&f), &[], &[]);
        assert_eq!(rows.len(), 1, "the finding must NOT be dropped");
        assert!(
            rows[0].summary.contains("UNVERIFIED"),
            "row must be marked UNVERIFIED: {}",
            rows[0].summary
        );
    }

    /// A CONFIRMED finding renders exactly as before (no marker) — the label
    /// is only added for the non-confirmed verdicts.
    #[test]
    fn confirmed_finding_row_carries_no_verdict_marker() {
        let f = finding("F-C", 200).with_verdict(AuditVerdict::Confirmed);
        let rows = build_queue(&[], &[], std::slice::from_ref(&f), &[], &[]);
        assert_eq!(rows.len(), 1);
        assert!(
            !rows[0].summary.contains("UNVERIFIED") && !rows[0].summary.contains("REFUTED"),
            "confirmed row must not be marked: {}",
            rows[0].summary
        );
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

    /// CA-overwatch-001 (verified bypass): the representative was picked by
    /// newest `ts` alone, ignoring `verdict`. A later same-fingerprint
    /// Unverified/Refuted re-audit therefore SHADOWED an earlier Confirmed
    /// record, and `plan_finding_bridges` filters on the representative's
    /// verdict — so the whole group silently dropped out of the backlog
    /// bridge even though a Confirmed record genuinely exists in it.
    #[test]
    fn dedup_representative_prefers_confirmed_over_a_later_shadowing_verdict() {
        let confirmed_old = finding("F-1", 100); // Confirmed by default (older).
        let unverified_new = finding("F-1-DUP", 200).with_verdict(AuditVerdict::Unverified); // same fingerprint, newer.
        let deduped = dedup_findings(&[confirmed_old, unverified_new]);
        assert_eq!(
            deduped.len(),
            1,
            "same-fingerprint records must collapse to one group"
        );
        assert_eq!(
            deduped[0].0.verdict,
            AuditVerdict::Confirmed,
            "a later non-actionable verdict must not shadow a Confirmed record in the same group"
        );
        assert_eq!(
            deduped[0].1, 2,
            "occurrence count is unaffected by verdict priority"
        );
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

    // --- undetermined sources (t3) -------------------------------------------

    fn undet(meta: SourceMeta, effect: SourceEffect) -> UndeterminedSource {
        UndeterminedSource {
            meta,
            effect,
            why: "the ledger could not be read".to_string(),
        }
    }

    /// `Known` contributes its rows and records nothing; `Undetermined`
    /// contributes no rows but records WHY, so no channel can render the empty
    /// vec as an observation of zero.
    #[test]
    fn resolve_source_records_undetermined_instead_of_collapsing_to_empty() {
        let mut sink = Vec::new();
        let rows = resolve_source(
            Determination::known(vec![rb("overwatch", 10)]),
            SRC_ROLLBACK,
            &mut sink,
        );
        assert_eq!(rows.len(), 1, "a read source contributes its rows");
        assert!(sink.is_empty(), "a read source records nothing");

        let rows: Vec<RollbackEvent> =
            resolve_source(Determination::undetermined("boom"), SRC_ROLLBACK, &mut sink);
        assert!(rows.is_empty());
        assert_eq!(sink.len(), 1, "an unread source must be recorded");
        assert_eq!(sink[0].meta.tag, "rollback");
        assert_eq!(sink[0].effect, SourceEffect::Omitted);
        assert!(
            sink[0].why.contains("boom"),
            "the reason must be forwarded: {}",
            sink[0].why
        );
    }

    /// The keystone invariant: while any source is undetermined the row list is
    /// NEVER empty, so the "queue empty" prose is unreachable — and `--since` /
    /// `--limit` cannot shed the marker row that says so. `limit = 0` and a
    /// `since` in the far future are the two ways a caller can empty the real
    /// rows; neither may take the marker with them.
    #[test]
    fn an_undetermined_source_always_leaves_a_row_behind() {
        let undetermined = vec![undet(SRC_ROLLBACK, SourceEffect::Omitted)];

        // No real rows at all (the case that used to print "queue empty").
        let rows = assemble(Vec::new(), &undetermined, 999);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].kind, EntryKind::UndeterminedSource);
        assert_eq!(rows[0].severity, Severity::High);
        assert_eq!(rows[0].ts, 999, "stamped now, so --since cannot shed it");
        assert_eq!(rows[0].identifier, "rollbacks.jsonl");

        // Real rows truncated to nothing by `--limit 0`: the marker survives
        // because it is prepended AFTER the cap.
        let mut capped = build_queue(&[], &[rb("p", 10)], &[], &[], &[]);
        // `--limit 0` reaches `run` as this exact call (a variable, not a
        // literal, so clippy sees the cap for what it is).
        let limit: usize = 0;
        capped.truncate(limit);
        let rows = assemble(capped, &undetermined, 999);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].kind, EntryKind::UndeterminedSource);
    }

    /// ANTI-VACUITY control for the invariant above: with every source read,
    /// `assemble` adds nothing at all — the marker must not become permanent
    /// furniture that fires on a healthy store.
    #[test]
    fn a_fully_read_queue_gets_no_marker_rows() {
        let real = build_queue(&[], &[rb("p", 10)], &[], &[], &[]);
        let rows = assemble(real.clone(), &[], 999);
        assert_eq!(rows, real, "nothing may be injected when all sources read");
        assert!(assemble(Vec::new(), &[], 999).is_empty());
    }

    /// The empty-queue sentence may only name sources that were actually read.
    #[test]
    fn empty_queue_line_never_names_a_source_it_could_not_read() {
        // Everything read → the historical sentence, unchanged.
        let all_read = empty_queue_line(&[]);
        assert!(
            all_read.contains("review queue empty")
                && all_read.contains("systemic violations")
                && all_read.contains("rollbacks")
                && all_read.contains("merge conflicts"),
            "a genuinely empty queue still enumerates its sources: {all_read}"
        );

        // Rollbacks undetermined → the word must be gone from the "no ..." list.
        let line = empty_queue_line(&[undet(SRC_ROLLBACK, SourceEffect::Omitted)]);
        assert!(
            !line.contains("rollbacks"),
            "an unread source must not be named as absent: {line}"
        );
        assert!(
            line.contains("findings") && line.contains("escalations"),
            "the sources that WERE read are still reported empty: {line}"
        );
        assert!(
            line.contains("NOT a report that the queue is empty"),
            "the caveat must be present: {line}"
        );

        // Nothing readable at all → no source may be named as absent.
        let all_undetermined: Vec<UndeterminedSource> = [
            SRC_SYSTEMIC,
            SRC_ROLLBACK,
            SRC_AI_FINDING,
            SRC_ESCALATION,
            SRC_MERGE_CONFLICT,
        ]
        .into_iter()
        .map(|m| undet(m, SourceEffect::Omitted))
        .collect();
        let line = empty_queue_line(&all_undetermined);
        assert!(
            line.contains("NO source could be read"),
            "with nothing read, the line must say exactly that: {line}"
        );
    }

    /// The two effects are different events and must not render as one: an
    /// omitted source hides rows, an unfiltered one shows too many.
    #[test]
    fn omitted_and_unfiltered_sources_are_reported_differently() {
        let omitted = undet(SRC_ROLLBACK, SourceEffect::Omitted);
        assert!(omitted.warning().contains("OMITTED"));
        assert!(omitted.warning().contains("NOT a report of zero rollbacks"));
        assert!(omitted.row(1).summary.contains("SOURCE NOT SHOWN"));

        let unfiltered = undet(SRC_MERGE_RESOLUTION, SourceEffect::ShownUnfiltered);
        assert!(
            unfiltered.warning().contains("UNRESOLVED")
                && !unfiltered.warning().contains("OMITTED"),
            "an unfiltered source is not an omitted one: {}",
            unfiltered.warning()
        );
        assert!(unfiltered.row(1).summary.contains("SOURCE NOT FILTERED"));
        assert_eq!(
            unfiltered.row(1).identifier,
            "merge_conflict_resolutions.jsonl"
        );
    }

    #[test]
    fn undetermined_source_kind_serializes_kebab_case() {
        // The `kind` a `--json` consumer greps for.
        let j = serde_json::to_string(&EntryKind::UndeterminedSource).unwrap();
        assert_eq!(j, "\"undetermined-source\"");
    }

    #[test]
    fn source_health_maps_undetermined_to_exit_three() {
        assert_eq!(SourceHealth::AllRead.exit_code(), 0);
        assert_eq!(SourceHealth::SomeUndetermined.exit_code(), 3);
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
