/// Review-effectiveness measurement: human dispositions of AI/adversarial
/// review findings (`crate::review_finding::ReviewFinding`).
///
/// Context: `review_finding.rs` records that a finding entered the human
/// review queue (`{finding_id, source, severity, summary, file, ts}`), but
/// nothing about what a human did with it afterward. Without that outcome we
/// cannot measure whether the queue is actually useful: how often findings
/// are false positives, how often a human agrees the finding was real, or how
/// long items sit unresolved before disposition.
///
/// This module defines the schema (`Disposition`) and PURE metric functions
/// that compute false-positive rate / agreement rate / median latency /
/// closure rate from a disposition ledger joined against the findings store.
/// Closure is the one measured against the FINDINGS rather than the ledger, so
/// it is defined even when nothing has been dispositioned at all — which is
/// exactly the state ("queued, never closed") the other three cannot express.
/// It mirrors
/// `audit_round.rs`: data + pure computation only, no I/O (see `store.rs` for
/// the append-only JSONL persistence and `disposition_cli.rs` for the
/// fail-soft CLI glue).
use crate::review_finding::ReviewFinding;
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

/// The human verdict on an AI/adversarial review finding.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DispositionVerdict {
    /// The human agrees the finding is real (the AI was right).
    Confirmed,
    /// The human reviewed it and chose not to act on it (e.g. accepted risk,
    /// duplicate, out of scope) without disputing its accuracy.
    Dismissed,
    /// The human determined the finding was NOT real (the AI was wrong).
    FalsePositive,
}

impl DispositionVerdict {
    /// Parse a CLI-facing verdict string: `confirmed`, `dismissed`, or
    /// `false-positive` (hyphenated, matching the `--verdict` flag). Unknown
    /// values are rejected with a clear error rather than silently defaulting.
    pub fn parse_cli(raw: &str) -> Result<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "confirmed" => Ok(Self::Confirmed),
            "dismissed" => Ok(Self::Dismissed),
            "false-positive" | "false_positive" => Ok(Self::FalsePositive),
            other => Err(anyhow!(
                "unknown disposition verdict {other:?}: expected confirmed | dismissed | false-positive"
            )),
        }
    }

    /// The canonical snake_case label used in JSON output (`by_verdict` keys).
    pub fn label(&self) -> &'static str {
        match self {
            Self::Confirmed => "confirmed",
            Self::Dismissed => "dismissed",
            Self::FalsePositive => "false_positive",
        }
    }
}

/// A single recorded human disposition of a review finding.
///
/// Joins to its originating [`ReviewFinding`] by `finding_id` — the
/// queue-entry timestamp is NOT duplicated here (see [`median_latency_secs`]),
/// so `Disposition` on its own only carries the resolution side of the story.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Disposition {
    /// The finding_id this disposition resolves (joins to a `ReviewFinding`).
    pub finding_id: String,
    /// The human verdict.
    pub verdict: DispositionVerdict,
    /// Free-text identifier of who resolved it (a person, or a tool name).
    pub reviewer: String,
    /// Unix timestamp when the disposition was recorded.
    pub resolved_ts: i64,
}

impl Disposition {
    /// Construct a disposition record.
    pub fn new(
        finding_id: String,
        verdict: DispositionVerdict,
        reviewer: String,
        resolved_ts: i64,
    ) -> Self {
        Self {
            finding_id,
            verdict,
            reviewer,
            resolved_ts,
        }
    }
}

/// `numer / denom` as a fraction in `[0,1]`, or `None` when `denom == 0`
/// (undefined — nothing to measure yet, guarding a division-by-zero panic).
fn rate(numer: usize, denom: usize) -> Option<f64> {
    if denom == 0 {
        None
    } else {
        Some(numer as f64 / denom as f64)
    }
}

/// False-positive rate = count(FalsePositive) / total dispositions, in
/// `[0,1]`. `None` on an empty ledger (undefined, not zero — there is no rate
/// to report yet).
pub fn false_positive_rate(dispositions: &[Disposition]) -> Option<f64> {
    let fp = dispositions
        .iter()
        .filter(|d| d.verdict == DispositionVerdict::FalsePositive)
        .count();
    rate(fp, dispositions.len())
}

/// Human-agreement rate = count(Confirmed) / total dispositions, in `[0,1]`.
///
/// Definition: this measures how often a human, upon reviewing an
/// AI/adversarial finding, CONFIRMS it was a real, actionable finding (i.e.
/// the human agrees with the AI). `Dismissed` (reviewed but not acted on,
/// without disputing accuracy) and `FalsePositive` (the AI was wrong) both
/// count against agreement in this denominator-inclusive definition — only
/// an explicit `Confirmed` counts as agreement. `None` on an empty ledger.
pub fn agreement_rate(dispositions: &[Disposition]) -> Option<f64> {
    let confirmed = dispositions
        .iter()
        .filter(|d| d.verdict == DispositionVerdict::Confirmed)
        .count();
    rate(confirmed, dispositions.len())
}

/// Closure counts per finding `source`: `source -> (closed, total)`.
///
/// A finding is CLOSED when a disposition exists for its `finding_id` — any
/// verdict, since `dismissed` and `false_positive` are resolutions too. Every
/// other finding is OPEN. The denominator is the DISTINCT finding ids in
/// `findings`, and the numerator is computed by INTERSECTION rather than by
/// counting dispositions: a disposition whose `finding_id` joins to no known
/// finding (a hand-recorded id, a typo, a finding whose stream was lost) would
/// otherwise inflate the numerator and could push the rate above 1.0 —
/// "measured better than perfect", which is how a metric stops being read.
///
/// Split by source on purpose. A single fleet-wide number cannot answer "are
/// THIS producer's findings ever getting closed", which is the question that
/// went unanswerable while one producer recorded no dispositions at all.
///
/// Not to be confused with [`crate::audit_round::closure_rate`], a different
/// ratio over a different denominator (regression tests per confirmed finding
/// within one Continuous-Audit round).
pub fn closure_by_source(
    dispositions: &[Disposition],
    findings: &[ReviewFinding],
) -> std::collections::BTreeMap<String, (usize, usize)> {
    let disposed: std::collections::HashSet<&str> =
        dispositions.iter().map(|d| d.finding_id.as_str()).collect();
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut out: std::collections::BTreeMap<String, (usize, usize)> =
        std::collections::BTreeMap::new();
    for f in findings {
        // The same finding can appear twice (hot plus archive, or a re-append);
        // counting it twice would distort both sides of the ratio.
        if !seen.insert(f.finding_id.as_str()) {
            continue;
        }
        let entry = out.entry(f.source.clone()).or_insert((0, 0));
        entry.1 += 1;
        if disposed.contains(f.finding_id.as_str()) {
            entry.0 += 1;
        }
    }
    out
}

/// Closure rate = findings with a disposition / all known findings, in `[0,1]`.
///
/// `None` when no findings are known at all — undefined, NOT zero. Zero would
/// read as "nothing is ever closed", which is a damning claim to make about an
/// empty store; the two must stay distinguishable. See [`closure_by_source`]
/// for how the numerator avoids counting unjoinable dispositions.
pub fn closure_rate(dispositions: &[Disposition], findings: &[ReviewFinding]) -> Option<f64> {
    let by_source = closure_by_source(dispositions, findings);
    let closed: usize = by_source.values().map(|(c, _)| *c).sum();
    let total: usize = by_source.values().map(|(_, t)| *t).sum();
    rate(closed, total)
}

/// Median latency (seconds) from a finding entering the review queue to its
/// disposition, across all dispositions that join to a known finding.
///
/// Join: each [`Disposition`] joins to its [`ReviewFinding`] by `finding_id`.
/// The finding's recorded `ts` is the single source of truth for
/// queue-entry time (never duplicated into `Disposition`). A disposition
/// whose `finding_id` has no matching finding is skipped (fail-soft — an
/// orphaned disposition contributes nothing rather than erroring).
///
/// Multiple-match rule: if a `finding_id` matches MORE THAN ONE finding
/// record (e.g. the Continuous-Audit loop re-recorded the same still-open
/// finding across rounds — see `review_queue`'s dedup test), the join uses
/// the EARLIEST `ts` among the matches, since that is when the finding first
/// entered the queue (first-seen), not when it was most recently re-affirmed.
///
/// Even-count median convention: this function sorts the per-disposition
/// latencies and, for an EVEN count, returns the MEAN of the two middle
/// values using integer (truncating) division — i.e. `(a + b) / 2` on `i64`.
/// For an ODD count it returns the single middle value exactly. `None` when
/// there are zero joinable (finding_id, disposition) pairs.
pub fn median_latency_secs(
    dispositions: &[Disposition],
    findings: &[ReviewFinding],
) -> Option<i64> {
    let mut latencies: Vec<i64> = Vec::with_capacity(dispositions.len());
    for d in dispositions {
        let earliest_ts = findings
            .iter()
            .filter(|f| f.finding_id == d.finding_id)
            .map(|f| f.ts)
            .min();
        if let Some(entry_ts) = earliest_ts {
            latencies.push(d.resolved_ts - entry_ts);
        }
        // else: orphaned disposition (no matching finding) — skip, fail-soft.
    }

    if latencies.is_empty() {
        return None;
    }
    latencies.sort_unstable();
    let n = latencies.len();
    if n % 2 == 1 {
        Some(latencies[n / 2])
    } else {
        let a = latencies[n / 2 - 1];
        let b = latencies[n / 2];
        Some((a + b) / 2)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn disp(id: &str, verdict: DispositionVerdict, resolved_ts: i64) -> Disposition {
        Disposition::new(id.to_string(), verdict, "alice".to_string(), resolved_ts)
    }

    fn finding_from(id: &str, source: &str) -> ReviewFinding {
        ReviewFinding::new(
            id.to_string(),
            source.to_string(),
            None,
            "s".to_string(),
            None,
            None,
            10,
        )
    }

    #[test]
    fn closure_splits_by_source_and_counts_every_verdict_as_closed() {
        let findings = vec![
            finding_from("specguard:spec-drift:a", "specguard"),
            finding_from("specguard:spec-drift:b", "specguard"),
            finding_from("rg-1", "reviewgate"),
        ];
        // Dismissed and false-positive are resolutions too — a finding a human
        // reviewed and rejected is closed, not still waiting.
        let dispositions = vec![
            disp("specguard:spec-drift:a", DispositionVerdict::Confirmed, 20),
            disp("rg-1", DispositionVerdict::FalsePositive, 20),
        ];
        let by_source = closure_by_source(&dispositions, &findings);
        assert_eq!(by_source.get("specguard"), Some(&(1, 2)));
        assert_eq!(by_source.get("reviewgate"), Some(&(1, 1)));
        assert_eq!(closure_rate(&dispositions, &findings), Some(2.0 / 3.0));
    }

    /// A disposition that joins to no known finding must not inflate the
    /// numerator — the rate has to stay inside `[0,1]` or it stops being read.
    #[test]
    fn an_unjoinable_disposition_cannot_push_the_rate_above_one() {
        let findings = vec![finding_from("f-1", "specguard")];
        let dispositions = vec![
            disp("f-1", DispositionVerdict::Confirmed, 20),
            disp("ghost-typo", DispositionVerdict::Confirmed, 20),
            disp("another-ghost", DispositionVerdict::Dismissed, 20),
        ];
        assert_eq!(closure_rate(&dispositions, &findings), Some(1.0));
    }

    /// The same finding appearing twice (hot plus archive, or a re-append) must
    /// not be counted twice on either side of the ratio.
    #[test]
    fn a_duplicated_finding_row_is_counted_once() {
        let findings = vec![
            finding_from("f-1", "specguard"),
            finding_from("f-1", "specguard"),
            finding_from("f-2", "specguard"),
        ];
        let dispositions = vec![disp("f-1", DispositionVerdict::Confirmed, 20)];
        assert_eq!(closure_by_source(&dispositions, &findings), {
            let mut m = std::collections::BTreeMap::new();
            m.insert("specguard".to_string(), (1, 2));
            m
        });
    }

    /// ANTI-VACUITY CONTROL: no findings at all is UNDEFINED. Kept separate from
    /// the zero case below so it stays true of a metric that computes nothing —
    /// it is the assertion that must NOT distinguish a working implementation.
    #[test]
    fn closure_of_an_empty_store_is_undefined() {
        assert_eq!(closure_rate(&[], &[]), None);
    }

    /// ...whereas findings that exist and were never dispositioned is a real
    /// 0.0, not `None`. Collapsing the two would hide an entirely unclosed
    /// queue behind the same "n/a" an empty store prints.
    #[test]
    fn findings_with_no_dispositions_are_a_real_zero_not_undefined() {
        let findings = vec![finding_from("f-1", "specguard")];
        assert_eq!(closure_rate(&[], &findings), Some(0.0));
    }

    #[test]
    fn parse_cli_accepts_known_verdicts_and_rejects_unknown() {
        assert_eq!(
            DispositionVerdict::parse_cli("confirmed").unwrap(),
            DispositionVerdict::Confirmed
        );
        assert_eq!(
            DispositionVerdict::parse_cli("dismissed").unwrap(),
            DispositionVerdict::Dismissed
        );
        assert_eq!(
            DispositionVerdict::parse_cli("false-positive").unwrap(),
            DispositionVerdict::FalsePositive
        );
        assert_eq!(
            DispositionVerdict::parse_cli("  Confirmed  ").unwrap(),
            DispositionVerdict::Confirmed
        );
        assert!(DispositionVerdict::parse_cli("bogus").is_err());
    }

    #[test]
    fn disposition_round_trips_json_as_snake_case() {
        let d = disp("F-1", DispositionVerdict::FalsePositive, 100);
        let json = serde_json::to_string(&d).unwrap();
        assert!(
            json.contains("\"false_positive\""),
            "verdict must serialize snake_case: {json}"
        );
        let back: Disposition = serde_json::from_str(&json).unwrap();
        assert_eq!(d, back);
    }

    #[test]
    fn false_positive_rate_empty_is_none() {
        assert_eq!(false_positive_rate(&[]), None);
    }

    #[test]
    fn false_positive_rate_all_one_verdict() {
        let ds = vec![
            disp("a", DispositionVerdict::FalsePositive, 1),
            disp("b", DispositionVerdict::FalsePositive, 2),
        ];
        assert_eq!(false_positive_rate(&ds), Some(1.0));
    }

    #[test]
    fn false_positive_rate_mixed() {
        let ds = vec![
            disp("a", DispositionVerdict::FalsePositive, 1),
            disp("b", DispositionVerdict::Confirmed, 2),
            disp("c", DispositionVerdict::Dismissed, 3),
            disp("d", DispositionVerdict::FalsePositive, 4),
        ];
        assert_eq!(false_positive_rate(&ds), Some(0.5));
    }

    #[test]
    fn agreement_rate_empty_is_none() {
        assert_eq!(agreement_rate(&[]), None);
    }

    #[test]
    fn agreement_rate_all_confirmed() {
        let ds = vec![
            disp("a", DispositionVerdict::Confirmed, 1),
            disp("b", DispositionVerdict::Confirmed, 2),
        ];
        assert_eq!(agreement_rate(&ds), Some(1.0));
    }

    #[test]
    fn agreement_rate_mixed() {
        let ds = vec![
            disp("a", DispositionVerdict::Confirmed, 1),
            disp("b", DispositionVerdict::Dismissed, 2),
            disp("c", DispositionVerdict::FalsePositive, 3),
            disp("d", DispositionVerdict::Confirmed, 4),
        ];
        assert_eq!(agreement_rate(&ds), Some(0.5));
    }

    #[test]
    fn median_latency_empty_is_none() {
        assert_eq!(median_latency_secs(&[], &[]), None);
    }

    #[test]
    fn median_latency_odd_count_takes_middle() {
        let findings = vec![finding("a", 0), finding("b", 0), finding("c", 0)];
        let ds = vec![
            disp("a", DispositionVerdict::Confirmed, 10),
            disp("b", DispositionVerdict::Confirmed, 30),
            disp("c", DispositionVerdict::Confirmed, 20),
        ];
        // latencies: 10, 30, 20 -> sorted 10,20,30 -> middle = 20
        assert_eq!(median_latency_secs(&ds, &findings), Some(20));
    }

    #[test]
    fn median_latency_even_count_is_mean_of_two_middle_integer_division() {
        let findings = vec![
            finding("a", 0),
            finding("b", 0),
            finding("c", 0),
            finding("d", 0),
        ];
        let ds = vec![
            disp("a", DispositionVerdict::Confirmed, 10),
            disp("b", DispositionVerdict::Confirmed, 20),
            disp("c", DispositionVerdict::Confirmed, 31),
            disp("d", DispositionVerdict::Confirmed, 40),
        ];
        // latencies sorted: 10, 20, 31, 40 -> middle two: 20, 31 -> (20+31)/2 = 25 (int div)
        assert_eq!(median_latency_secs(&ds, &findings), Some(25));
    }

    #[test]
    fn median_latency_skips_dispositions_with_no_matching_finding() {
        let findings = vec![finding("a", 0)];
        let ds = vec![
            disp("a", DispositionVerdict::Confirmed, 10),
            disp("orphan", DispositionVerdict::Confirmed, 999),
        ];
        // Only "a" joins; "orphan" is skipped, so median is just its latency.
        assert_eq!(median_latency_secs(&ds, &findings), Some(10));
    }

    #[test]
    fn median_latency_returns_none_when_nothing_joins() {
        let findings = vec![finding("a", 0)];
        let ds = vec![disp("no-such-id", DispositionVerdict::Confirmed, 10)];
        assert_eq!(median_latency_secs(&ds, &findings), None);
    }

    #[test]
    fn median_latency_multiple_matches_uses_earliest_ts() {
        // "F-9" re-recorded across rounds (the review_queue dedup scenario):
        // first-seen ts=100, re-affirmed at ts=500. The join must use the
        // EARLIEST (100), so latency = resolved(150) - 100 = 50, not
        // 150 - 500 = negative.
        let findings = vec![finding("F-9", 500), finding("F-9", 100)];
        let ds = vec![disp("F-9", DispositionVerdict::Confirmed, 150)];
        assert_eq!(median_latency_secs(&ds, &findings), Some(50));
    }
}
