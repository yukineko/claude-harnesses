/// The review-queue→backlog drain: `overwatch review-queue --to-backlog`.
///
/// This closes the "discover→fix" loop by forwarding the **whole unified review
/// queue** — not just AI findings — to the backlog (one `backlog add` per
/// entry), so `/flow` can auto-repair each item. Two ledgers keep it idempotent,
/// deliberately kept separate:
///
/// * **AI findings** (`review_findings.jsonl`, the `record-finding` ingestion
///   point) take the enrichment-carrying path below: each not-yet-bridged
///   `finding_id` is forwarded with triage signals in its notes (elapsed days,
///   verifier rationale, regression-test freshness) and recorded in
///   `bridged_findings.jsonl`. That ledger doubles as the review-metrics
///   "resolved finding" source, so it must stay keyed on bare finding-ids.
/// * **The other three streams** — systemic gate violations, canary rollbacks,
///   and condukt escalations — are assembled via [`review_queue::build_queue`]
///   and forwarded by the pure [`plan_entry_adds`] planner, keyed on a
///   composite `<kind-tag>:<identifier>` in a *separate* `bridged_entries.jsonl`
///   ledger so the finding-resolution logic above is untouched.
///
/// Idempotency is enforced by these ledgers, NOT by the backlog's own duplicate
/// guard (which hashes on title+project). A finding recurring across audit
/// rounds collapses to one row via [`review_queue::dedup_findings`]; a flapping
/// canary collapses to one row via the queue's same-plugin collapse.
///
/// Severity maps one tier hotter than a generic finding (high→p0, med→p1,
/// low→p2) because a gate-defense regression left unattended has outsized blast
/// radius; findings are tagged with their `source`, and non-finding entries
/// with their kind tag, so the backlog stays filterable (`backlog list --tag
/// rollback`).
///
/// **Fail-soft (never-break-a-turn):** a missing/empty/corrupt store, an absent
/// `backlog` binary, or a non-zero `backlog add` are each warned and skipped —
/// the command as a whole always succeeds (exit 0).
use crate::review_escalation;
use crate::review_finding::ReviewFinding;
use crate::review_queue::{self, EntryKind, ReviewQueueEntry, Severity};
use crate::store;
use crate::test_freshness::{self, TestFreshness};
use crate::violation::{self, RecurrencePolicy};
use anyhow::Result;
use std::collections::HashSet;
use std::path::Path;

/// Resolve the `backlog` binary: an explicit `OVERWATCH_BACKLOG_BIN` override
/// (used to wire a specific binary, and by tests) wins; otherwise the first
/// `backlog` on `PATH`. Returns `None` when neither resolves (fail-soft: the
/// caller then skips the bridge without erroring).
fn resolve_backlog_bin() -> Option<String> {
    if let Ok(p) = std::env::var("OVERWATCH_BACKLOG_BIN") {
        if !p.is_empty() {
            return Some(p);
        }
    }
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let cand = dir.join("backlog");
        if cand.is_file() {
            return Some(cand.to_string_lossy().into_owned());
        }
    }
    None
}

/// Map a reviewer severity to a backlog priority tier: `high` → p0, `medium`/
/// `med` → p1, `low` → p2. Gate-defense findings carry outsized risk if left
/// unattended, so the whole scale is one tier hotter than a generic finding.
/// Unknown/absent severity defaults to p1 (the safe middle) rather than the
/// lowest tier, so an unclassified gate finding is never silently buried.
fn severity_to_priority(severity: Option<&str>) -> &'static str {
    match severity.map(|s| s.to_ascii_lowercase()).as_deref() {
        Some("high") => "p0",
        Some("low") => "p2",
        // "medium"/"med", unknown, and absent all land in the middle tier.
        _ => "p1",
    }
}

/// The [`Severity`]-enum counterpart to [`severity_to_priority`], used for the
/// non-finding streams whose severity is already normalized to the enum by
/// [`review_queue::build_queue`] (systemic/rollback/escalation are always
/// `High` → p0; the medium/low arms exist for completeness and to stay in
/// lockstep with the string mapping). Same rationale: gate-defense signals ride
/// one tier hotter than a generic finding.
fn queue_severity_to_priority(severity: Severity) -> &'static str {
    match severity {
        Severity::High => "p0",
        Severity::Medium => "p1",
        Severity::Low => "p2",
    }
}

/// One planned `backlog add` derived from a non-finding review-queue entry
/// (systemic / rollback / escalation). Produced purely by [`plan_entry_adds`]
/// so the enqueue decision is unit-testable without spawning `backlog`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PlannedAdd {
    /// Idempotency key `<kind-tag>:<identifier>`, recorded in the
    /// bridged-entries ledger after a successful add.
    key: String,
    /// Backlog task title — the entry's human-readable summary.
    title: String,
    /// Priority tier from the entry's severity.
    priority: &'static str,
    /// The kind tag (`systemic`/`rollback`/`escalation`), so the backlog stays
    /// filterable back to the stream it came from (`backlog list --tag rollback`).
    tag: &'static str,
    /// Machine-readable provenance line for the backlog task notes.
    notes: String,
}

/// Pure planner: given the non-finding review-queue rows and the set of
/// already-bridged keys, decide which rows to forward and how. AI-finding rows
/// (if any slip in) are excluded — that stream is bridged by the richer,
/// enrichment-carrying path in [`run_in`] and tracked in a separate ledger.
/// Deterministic: input order is preserved and nothing depends on hash order.
fn plan_entry_adds(rows: &[ReviewQueueEntry], already: &HashSet<String>) -> Vec<PlannedAdd> {
    rows.iter()
        .filter(|r| r.kind != EntryKind::AiFinding)
        .filter_map(|r| {
            let key = format!("{}:{}", r.kind.tag(), r.identifier);
            if already.contains(&key) {
                return None; // idempotent: already forwarded in a prior run.
            }
            Some(PlannedAdd {
                title: r.summary.clone(),
                priority: queue_severity_to_priority(r.severity),
                tag: r.kind.tag(),
                notes: format!(
                    "kind:{} identifier:{} occurrences:{}",
                    r.kind.tag(),
                    r.identifier,
                    r.occurrences
                ),
                key,
            })
        })
        .collect()
}

/// Build the backlog task notes for one finding: the original
/// finding-id/file/severity line, plus advisory triage signals a human uses to
/// decide fix-vs-wontfix — elapsed time since confirmation (a stale finding
/// with no incident since is a deprioritize signal), the verifier's
/// `rationale` if recorded, and a regression-test freshness check (if a
/// matching `#[ignore]`d test could be reverse-looked-up and run). None of
/// these change `to_backlog`'s dedup/idempotency logic — they only thicken the
/// notes string.
fn build_notes(f: &ReviewFinding, now: i64, freshness: Option<&TestFreshness>) -> String {
    let mut notes = format!(
        "finding-id:{} file:{} severity:{}",
        f.finding_id,
        f.file.as_deref().unwrap_or("(none)"),
        f.severity.as_deref().unwrap_or("(none)"),
    );

    let elapsed_days = (now - f.ts).max(0) / 86_400;
    notes.push_str(&format!(" | confirmed: {elapsed_days}日前"));

    if let Some(rationale) = f.rationale.as_deref() {
        notes.push_str(&format!(" | rationale: {rationale}"));
    }

    match freshness {
        Some(TestFreshness::Failing { message }) => {
            notes.push_str(&format!(" | regression test: FAIL: {message}"));
        }
        Some(TestFreshness::Passing) => {
            notes.push_str(" | regression test: PASS（解消済みの可能性）");
        }
        // NotFound/ExecutionError and "no test looked up at all" are all
        // inconclusive from the triage reader's perspective — fold them into
        // one "no applicable test" line rather than surfacing internal states.
        Some(TestFreshness::NotFound) | Some(TestFreshness::ExecutionError) | None => {
            notes.push_str(" | regression test: 該当テストなし");
        }
    }

    notes
}

/// Pure planner: given the deduped representative findings, the raw finding
/// records, and the already-bridged ledger (bare finding-ids — also the
/// review-metrics "resolved" source), decide which representatives still need a
/// `backlog add`.
///
/// Idempotency keys on the **stable fingerprint**, not the rotating
/// representative id. [`review_queue::dedup_findings`] collapses a recurring
/// finding to ONE group whose representative is the newest (`ts`) record, so a
/// finding re-recorded under a fresh `finding_id` each audit round rotates the
/// representative id; an id-only check would then fail to match the prior
/// round's ledger entry and re-bridge a DUPLICATE. Instead we take the
/// fingerprints of every raw record whose bare id is already in the ledger — the
/// recurring finding's prior record persists in `review_findings.jsonl` and
/// shares this fingerprint — so the group is recognized as already-bridged
/// however the representative id rotates. Deterministic (input order preserved,
/// no hash-order dependence). The ledger contract is untouched: callers still
/// record the bare representative `finding_id`.
fn plan_finding_bridges<'a>(
    deduped: &'a [(ReviewFinding, u32)],
    raw_findings: &[ReviewFinding],
    already: &HashSet<String>,
) -> Vec<&'a ReviewFinding> {
    let bridged_fingerprints: HashSet<String> = raw_findings
        .iter()
        .filter(|f| already.contains(&f.finding_id))
        .map(review_queue::finding_fingerprint)
        .collect();

    deduped
        .iter()
        .filter(|(f, _)| {
            !already.contains(&f.finding_id)
                && !bridged_fingerprints.contains(&review_queue::finding_fingerprint(f))
        })
        .map(|(f, _)| f)
        .collect()
}

/// Read the confirmed findings, forward each not-yet-bridged one to the backlog,
/// and record the successful bridges. Always returns `Ok(())` (fail-soft).
pub fn to_backlog() -> Result<()> {
    let cwd = std::env::current_dir()?;
    run_in(&cwd)
}

/// The bridge over an explicit project dir (factored out so the logic is
/// testable without depending on the process cwd).
fn run_in(cwd: &Path) -> Result<()> {
    // 1. Confirmed findings, collapsed to newest-per-id.
    let findings = store::read_review_findings(cwd).unwrap_or_default();
    let deduped = review_queue::dedup_findings(&findings);

    // 1b. The other three review-queue streams — systemic gate violations,
    // canary rollbacks, and condukt escalations — assembled into their queue
    // rows (findings passed empty here; they take the richer path above). This
    // is the "consolidation": `--to-backlog` now drains the WHOLE unified
    // review queue, not just AI findings.
    let events = store::read_violations(cwd).unwrap_or_default();
    let systemic: Vec<_> =
        violation::detect_recurrence(&events, store::now(), RecurrencePolicy::default())
            .into_iter()
            .filter(|r| r.is_systemic)
            .collect();
    let rollbacks = store::read_rollbacks(cwd).unwrap_or_default();
    let escalations = review_escalation::read_open_escalations(cwd);
    let entry_rows = review_queue::build_queue(&systemic, &rollbacks, &[], &escalations);

    // 2. Already-bridged sets — findings keyed on bare finding_id (also the
    // review-metrics "resolved" source), non-finding entries keyed on the
    // composite `<kind-tag>:<identifier>` in their own separate ledger.
    let already_findings: HashSet<String> = store::read_bridged_findings(cwd)
        .unwrap_or_default()
        .into_iter()
        .collect();
    let already_entries: HashSet<String> = store::read_bridged_entries(cwd)
        .unwrap_or_default()
        .into_iter()
        .collect();
    let planned = plan_entry_adds(&entry_rows, &already_entries);

    // Which deduped findings still need bridging — keyed on the STABLE
    // fingerprint, not the rotating dedup representative id (CA-overwatch-01).
    let to_bridge = plan_finding_bridges(&deduped, &findings, &already_findings);

    // 3. Backlog binary (fail-soft when absent).
    let backlog = match resolve_backlog_bin() {
        Some(b) => b,
        None => {
            eprintln!(
                "overwatch: WARNING backlog binary not found (OVERWATCH_BACKLOG_BIN unset, none on PATH) — skipping bridge, continuing"
            );
            println!(
                "{}",
                serde_json::json!({
                    "bridged": 0,
                    "considered": deduped.len(),
                    "entries_bridged": 0,
                    "entries_considered": planned.len(),
                    "skipped": "backlog-unavailable"
                })
            );
            return Ok(());
        }
    };

    let project = cwd.to_string_lossy().into_owned();
    let now = store::now();
    let mut bridged_now = 0usize;

    for &f in &to_bridge {
        let priority = severity_to_priority(f.severity.as_deref());
        // Regression-test freshness (fail-soft): reverse-lookup a
        // `#[ignore = "<finding-id>: ..."]` test and re-run it. Any failure to
        // find/run one (no matching test, cargo/crate unavailable) just
        // leaves `freshness` as `None`, folded into "no test" in the notes.
        let freshness = test_freshness::find_ignored_test(&f.finding_id, cwd).map(
            |(crate_name, _test_path, fn_name)| {
                test_freshness::run_ignored_test(&crate_name, &fn_name)
            },
        );
        let notes = build_notes(f, now, freshness.as_ref());

        let status = std::process::Command::new(&backlog)
            .arg("add")
            .arg("--title")
            .arg(&f.summary)
            .arg("--project")
            .arg(&project)
            .arg("--priority")
            .arg(priority)
            // Tag with the finding's source (e.g. `continuous-audit`) so the
            // backlog stays filterable back to where it came from
            // (`backlog list --tag continuous-audit`).
            .arg("--tag")
            .arg(&f.source)
            .arg("--notes")
            .arg(&notes)
            .status();

        match status {
            Ok(s) if s.success() => match store::append_bridged_finding(cwd, &f.finding_id) {
                Ok(()) => bridged_now += 1,
                Err(e) => {
                    // The backlog task WAS added but we could not persist the
                    // idempotency key. Warn; a future round may re-add it (the
                    // backlog's title+project guard is the backstop there).
                    eprintln!(
                        "overwatch: WARNING bridged finding {} but could not record it (continuing): {e}",
                        f.finding_id
                    );
                }
            },
            Ok(s) => {
                eprintln!(
                    "overwatch: WARNING `backlog add` failed for finding {} (exit {:?}) — continuing",
                    f.finding_id,
                    s.code()
                );
            }
            Err(e) => {
                eprintln!(
                    "overwatch: WARNING could not spawn backlog for finding {} (continuing): {e}",
                    f.finding_id
                );
            }
        }
    }

    // The three non-finding streams. Same fail-soft contract as the finding
    // loop: a failed/unspawnable `backlog add` is warned and skipped, and the
    // idempotency key is only recorded after a successful add.
    let mut entries_bridged_now = 0usize;
    for p in &planned {
        let status = std::process::Command::new(&backlog)
            .arg("add")
            .arg("--title")
            .arg(&p.title)
            .arg("--project")
            .arg(&project)
            .arg("--priority")
            .arg(p.priority)
            .arg("--tag")
            .arg(p.tag)
            .arg("--notes")
            .arg(&p.notes)
            .status();

        match status {
            Ok(s) if s.success() => match store::append_bridged_entry(cwd, &p.key) {
                Ok(()) => entries_bridged_now += 1,
                Err(e) => {
                    eprintln!(
                        "overwatch: WARNING bridged entry {} but could not record it (continuing): {e}",
                        p.key
                    );
                }
            },
            Ok(s) => {
                eprintln!(
                    "overwatch: WARNING `backlog add` failed for entry {} (exit {:?}) — continuing",
                    p.key,
                    s.code()
                );
            }
            Err(e) => {
                eprintln!(
                    "overwatch: WARNING could not spawn backlog for entry {} (continuing): {e}",
                    p.key
                );
            }
        }
    }

    println!(
        "{}",
        serde_json::json!({
            "bridged": bridged_now,
            "considered": deduped.len(),
            "entries_bridged": entries_bridged_now,
            "entries_considered": planned.len(),
        })
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_maps_high_p0_medium_p1_low_p2() {
        assert_eq!(severity_to_priority(Some("high")), "p0");
        assert_eq!(severity_to_priority(Some("HIGH")), "p0");
        assert_eq!(severity_to_priority(Some("medium")), "p1");
        assert_eq!(severity_to_priority(Some("med")), "p1");
        assert_eq!(severity_to_priority(Some("low")), "p2");
        // Unknown/absent severity defaults to the safe middle, not the floor.
        assert_eq!(severity_to_priority(Some("weird")), "p1");
        assert_eq!(severity_to_priority(None), "p1");
    }

    #[test]
    fn backlog_bin_override_env_wins() {
        std::env::set_var("OVERWATCH_BACKLOG_BIN", "/some/fake/backlog");
        assert_eq!(resolve_backlog_bin().as_deref(), Some("/some/fake/backlog"));
        std::env::remove_var("OVERWATCH_BACKLOG_BIN");
    }

    fn finding(rationale: Option<&str>, ts: i64) -> ReviewFinding {
        ReviewFinding::new(
            "F-1".to_string(),
            "reviewgate".to_string(),
            Some("high".to_string()),
            "unchecked unwrap".to_string(),
            Some("src/foo.rs".to_string()),
            rationale.map(str::to_string),
            ts,
        )
    }

    #[test]
    fn build_notes_includes_elapsed_days() {
        let f = finding(None, 0);
        let notes = build_notes(&f, 3 * 86_400, None);
        assert!(notes.contains("confirmed: 3日前"), "notes: {notes}");
    }

    #[test]
    fn build_notes_includes_rationale_when_present() {
        let f = finding(Some("foo.rs:42 unwraps a None"), 0);
        let notes = build_notes(&f, 0, None);
        assert!(
            notes.contains("rationale: foo.rs:42 unwraps a None"),
            "notes: {notes}"
        );
    }

    #[test]
    fn build_notes_omits_rationale_when_absent() {
        let f = finding(None, 0);
        let notes = build_notes(&f, 0, None);
        assert!(!notes.contains("rationale:"), "notes: {notes}");
    }

    #[test]
    fn build_notes_reports_failing_regression_test() {
        let f = finding(None, 0);
        let freshness = TestFreshness::Failing {
            message: "assertion failed".to_string(),
        };
        let notes = build_notes(&f, 0, Some(&freshness));
        assert!(
            notes.contains("regression test: FAIL: assertion failed"),
            "notes: {notes}"
        );
    }

    #[test]
    fn build_notes_reports_passing_regression_test() {
        let f = finding(None, 0);
        let notes = build_notes(&f, 0, Some(&TestFreshness::Passing));
        assert!(notes.contains("regression test: PASS"), "notes: {notes}");
    }

    #[test]
    fn build_notes_reports_no_applicable_test_when_none_found() {
        let f = finding(None, 0);
        let notes = build_notes(&f, 0, None);
        assert!(
            notes.contains("regression test: 該当テストなし"),
            "notes: {notes}"
        );
    }

    // --- non-finding stream planner (review-queue → backlog consolidation) ----

    fn entry(
        kind: EntryKind,
        severity: Severity,
        identifier: &str,
        summary: &str,
    ) -> ReviewQueueEntry {
        ReviewQueueEntry {
            kind,
            severity,
            ts: 100,
            summary: summary.to_string(),
            identifier: identifier.to_string(),
            occurrences: 1,
        }
    }

    #[test]
    fn queue_severity_maps_high_p0_medium_p1_low_p2() {
        assert_eq!(queue_severity_to_priority(Severity::High), "p0");
        assert_eq!(queue_severity_to_priority(Severity::Medium), "p1");
        assert_eq!(queue_severity_to_priority(Severity::Low), "p2");
    }

    #[test]
    fn plan_entry_adds_maps_each_kind_to_keyed_tagged_add() {
        let rows = vec![
            entry(
                EntryKind::Systemic,
                Severity::High,
                "blastguard:rm-rf",
                "sys summary",
            ),
            entry(
                EntryKind::Rollback,
                Severity::High,
                "overwatch",
                "rb summary",
            ),
            entry(
                EntryKind::Escalation,
                Severity::High,
                "esc-1",
                "esc summary",
            ),
        ];
        let planned = plan_entry_adds(&rows, &HashSet::new());
        assert_eq!(planned.len(), 3);

        // Keys are `<kind-tag>:<identifier>`, titles are the summaries, tags are
        // the kind tags, priority follows severity, notes carry provenance.
        assert_eq!(planned[0].key, "systemic:blastguard:rm-rf");
        assert_eq!(planned[0].tag, "systemic");
        assert_eq!(planned[0].title, "sys summary");
        assert_eq!(planned[0].priority, "p0");
        assert!(planned[0].notes.contains("kind:systemic"));
        assert!(planned[0].notes.contains("identifier:blastguard:rm-rf"));

        assert_eq!(planned[1].key, "rollback:overwatch");
        assert_eq!(planned[1].tag, "rollback");
        assert_eq!(planned[2].key, "escalation:esc-1");
        assert_eq!(planned[2].tag, "escalation");
    }

    #[test]
    fn plan_entry_adds_is_idempotent_against_already_bridged_keys() {
        let rows = vec![
            entry(EntryKind::Rollback, Severity::High, "overwatch", "rb"),
            entry(EntryKind::Systemic, Severity::High, "blastguard:x", "sys"),
        ];
        let mut already = HashSet::new();
        already.insert("rollback:overwatch".to_string());
        let planned = plan_entry_adds(&rows, &already);
        // The already-bridged rollback is skipped; the fresh systemic remains.
        assert_eq!(planned.len(), 1);
        assert_eq!(planned[0].key, "systemic:blastguard:x");
    }

    #[test]
    fn plan_entry_adds_excludes_ai_finding_rows() {
        // AI-finding rows are bridged by the enrichment-carrying path, not here;
        // even if one appears in the row slice it must be excluded.
        let rows = vec![
            entry(EntryKind::AiFinding, Severity::High, "F-1", "a finding"),
            entry(EntryKind::Rollback, Severity::High, "overwatch", "rb"),
        ];
        let planned = plan_entry_adds(&rows, &HashSet::new());
        assert_eq!(planned.len(), 1);
        assert_eq!(planned[0].tag, "rollback");
    }

    // --- stable-fingerprint idempotency (CA-overwatch-01) ---------------------

    fn recurring_finding(id: &str, ts: i64) -> ReviewFinding {
        // Same source + file + summary across rounds → same stable fingerprint;
        // only the id and ts change (the Continuous-Audit loop mints a fresh id
        // each round for a still-confirmed finding).
        ReviewFinding::new(
            id.to_string(),
            "continuous-audit".to_string(),
            Some("high".to_string()),
            "unchecked unwrap in foo".to_string(),
            Some("src/foo.rs".to_string()),
            None,
            ts,
        )
    }

    #[test]
    fn recurring_finding_under_rotating_representative_id_bridges_once() {
        // Round 1: only F-1 exists; nothing bridged yet → it is planned once and
        // the bridge records its bare id in the ledger (the review-metrics
        // "resolved" source).
        let f1 = recurring_finding("F-1", 100);
        let round1_raw = vec![f1.clone()];
        let round1_dedup = review_queue::dedup_findings(&round1_raw);
        let none_bridged = HashSet::new();
        let r1 = plan_finding_bridges(&round1_dedup, &round1_raw, &none_bridged);
        assert_eq!(r1.len(), 1);
        assert_eq!(r1[0].finding_id, "F-1");

        // Ledger now holds the bare representative id (contract preserved).
        let already: HashSet<String> = ["F-1".to_string()].into_iter().collect();

        // Round 2: the SAME underlying finding recurs under a FRESH id (newer
        // ts). dedup_findings collapses both into one group and ROTATES the
        // representative to the newest record (F-2).
        let f2 = recurring_finding("F-2", 200);
        let round2_raw = vec![f1.clone(), f2.clone()];
        let round2_dedup = review_queue::dedup_findings(&round2_raw);
        assert_eq!(
            round2_dedup.len(),
            1,
            "same fingerprint under different ids must collapse to one group"
        );
        assert_eq!(
            round2_dedup[0].0.finding_id, "F-2",
            "representative id rotates to the newest record"
        );

        // The recurring finding is recognized as already-bridged via its stable
        // fingerprint (NOT the rotated representative id F-2, which is absent
        // from the ledger) and is therefore NOT re-added. An id-only check
        // (the pre-fix behavior) would plan F-2 here and double-bridge.
        let r2 = plan_finding_bridges(&round2_dedup, &round2_raw, &already);
        assert!(
            r2.is_empty(),
            "recurring finding under a rotated representative id must not re-bridge: {:?}",
            r2.iter().map(|f| &f.finding_id).collect::<Vec<_>>()
        );
    }

    #[test]
    fn distinct_findings_still_bridge_independently() {
        // Guard against over-suppression: a genuinely different finding (distinct
        // fingerprint) is still planned even when another finding is bridged.
        let bridged = recurring_finding("F-1", 100);
        let other = ReviewFinding::new(
            "F-9".to_string(),
            "continuous-audit".to_string(),
            Some("high".to_string()),
            "different issue entirely".to_string(),
            Some("src/bar.rs".to_string()),
            None,
            150,
        );
        let raw = vec![bridged.clone(), other.clone()];
        let deduped = review_queue::dedup_findings(&raw);
        let already: HashSet<String> = ["F-1".to_string()].into_iter().collect();
        let planned = plan_finding_bridges(&deduped, &raw, &already);
        assert_eq!(planned.len(), 1);
        assert_eq!(planned[0].finding_id, "F-9");
    }
}
