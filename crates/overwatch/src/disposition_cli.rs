/// CLI-facing glue for review-effectiveness measurement (finding
/// dispositions + FP-rate / agreement-rate / median-latency metrics).
///
/// `record` appends one human disposition of an AI/adversarial review finding
/// (join key: `finding_id`, resolved against `crate::review_finding::ReviewFinding`);
/// `metrics` reads the disposition ledger back (joined against the findings
/// store) and prints the review-effectiveness report, human-readable or JSON.
///
/// Store writes are **fail-soft** (never-break-a-turn), matching
/// `audit_round_cli` / `rollback_cli::record_finding`: a store-write failure
/// is reported to stderr rather than propagated. An UNKNOWN `--verdict`
/// string, by contrast, is a genuine input error and is rejected with `Err`
/// (mirrors `DispositionVerdict::parse_cli`).
/// The metrics side is a REPORT, and a report computed from a ledger that could
/// not be read is not a low number — it is no number (t3). `metrics` therefore
/// reads both ledgers tri-state and, when either is undetermined, prints no
/// rate at all: the JSON keys stay present with `null` values (so no consumer
/// reads a `0` that was never measured) alongside an `undetermined_sources`
/// list, and the command exits 3.
use crate::disposition::{self, Disposition, DispositionVerdict};
use crate::reconcile::{self, ReconcileRange};
use crate::review_queue::SourceHealth;
use crate::store::{self, AppendOutcome};
use anyhow::Result;
use harness_core::verdict::Determination;

/// Commit-range `reconcile-fixed` itself uses from `continuous-audit.sh`
/// (`--last-n 200`) — kept identical here so `stale_undisposed_with_fix_commit`
/// answers "would a `reconcile-fixed` run right now find anything to do".
const STALE_SCAN_LAST_N: usize = 200;

/// Record one human disposition of a review finding.
///
/// `now` is the caller-supplied timestamp (the SAME `store::now()` source
/// `record-finding` / `audit-round record` use — passed in rather than read
/// here so this function stays testable without a wall clock). Fail-soft on
/// the store write: a write error is warned to stderr but returns `Ok(())`
/// so the caller is never broken by logging.
pub fn record(finding_id: String, verdict_str: &str, reviewer: String, now: i64) -> Result<()> {
    let verdict = DispositionVerdict::parse_cli(verdict_str)?;
    let cwd = std::env::current_dir()?;
    let record = Disposition::new(finding_id, verdict, reviewer, now);

    match store::append_disposition(&cwd, &record) {
        Ok(outcome) => {
            let (line, ok) = disposition_result(outcome, &record, verdict);
            println!("{line}");
            // Mirror the lease `begin` skip-JSON pattern: a contended HARD-SKIP
            // persisted nothing, so surface it with a nonzero exit rather than a
            // silent exit-0 success. `std::process::exit` is only reached on the
            // skip path (never in tests, which drive `disposition_result`).
            if !ok {
                std::process::exit(1);
            }
        }
        Err(e) => {
            eprintln!("overwatch: WARNING could not record disposition (continuing): {e}");
            println!(
                "{}",
                serde_json::json!({ "recorded": false, "reason": "store-write-failed" })
            );
        }
    }
    Ok(())
}

/// Build the `record-disposition` result line + success flag for a store
/// [`AppendOutcome`]. Pure/testable: keeps the truthfulness contract out of the
/// side-effecting `record()`. A contended HARD-SKIP is reported TRUTHFULLY as
/// `recorded:false` (with a contention note) and `ok=false` (→ nonzero exit),
/// NOT a phantom `recorded:true`, because nothing was persisted.
fn disposition_result(
    outcome: AppendOutcome,
    record: &Disposition,
    verdict: DispositionVerdict,
) -> (serde_json::Value, bool) {
    match outcome {
        AppendOutcome::Recorded => (
            serde_json::json!({
                "recorded": true,
                "finding_id": record.finding_id,
                "verdict": verdict.label(),
            }),
            true,
        ),
        AppendOutcome::SkippedContended => (
            serde_json::json!({
                "recorded": false,
                "reason": "lock_contended",
                "note": "store lock contended; disposition NOT persisted — retry shortly",
                "finding_id": record.finding_id,
            }),
            false,
        ),
    }
}

/// Read the disposition ledger (joined against the review-findings store) and
/// print the review-effectiveness report: false-positive rate, agreement
/// rate, median latency (seconds), and a per-verdict breakdown. Fail-soft: a
/// missing/empty store yields a zero/`None` report rather than an error.
pub fn metrics(json: bool) -> Result<SourceHealth> {
    let cwd = std::env::current_dir()?;
    let mut undetermined: Vec<&'static str> = Vec::new();
    let dispositions = match store::scan_dispositions(&cwd)? {
        Determination::Known(rows) => Some(rows),
        Determination::Undetermined(why) => {
            eprintln!(
                "overwatch review-metrics: WARNING — the disposition ledger could not be read \
                 or held an undecodable line ({why}); NO rate is computed from it. This is \
                 NOT a report of zero dispositions."
            );
            undetermined.push("dispositions.jsonl");
            None
        }
    };
    // Full history (hot plus archive): `compact_review_findings` may have moved
    // a resolved finding's record out of the hot store, but the latency join
    // below still needs it — see `store::scan_review_findings_all`.
    let findings = match store::scan_review_findings_all(&cwd)? {
        Determination::Known(rows) => Some(rows),
        Determination::Undetermined(why) => {
            eprintln!(
                "overwatch review-metrics: WARNING — the review-findings history (hot store \
                 plus archive) could not be read or held an undecodable line ({why}); NO \
                 closure rate is computed from it. This is NOT a report of zero findings."
            );
            undetermined.push("review_findings.jsonl");
            None
        }
    };

    let (dispositions, findings) = match (dispositions, findings) {
        (Some(d), Some(f)) => (d, f),
        // Refuse to print numbers derived from a ledger we could not read: a
        // zero here is indistinguishable from a measured zero, and the
        // measured zero ("queued and never closed") is a real signal this
        // report exists to show.
        _ => {
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "total": serde_json::Value::Null,
                        "false_positive_rate": serde_json::Value::Null,
                        "agreement_rate": serde_json::Value::Null,
                        "median_latency_secs": serde_json::Value::Null,
                        "by_verdict": serde_json::Value::Null,
                        "stale_undisposed_with_fix_commit": serde_json::Value::Null,
                        "closure_rate": serde_json::Value::Null,
                        "closure_by_source": serde_json::Value::Null,
                        "undetermined_sources": undetermined,
                    }))?
                );
            } else {
                println!(
                    "Review-effectiveness report: UNDETERMINED — {} could not be read, so no \
                     rate is computed (this is NOT a report of zero dispositions or zero \
                     findings)",
                    undetermined.join(", ")
                );
            }
            return Ok(SourceHealth::SomeUndetermined);
        }
    };

    let total = dispositions.len();
    let fp_rate = disposition::false_positive_rate(&dispositions);
    let agreement = disposition::agreement_rate(&dispositions);
    let median_latency = disposition::median_latency_secs(&dispositions, &findings);
    // Closure is measured against the FINDINGS, not the dispositions, so it is
    // the one figure here that is meaningful when nothing has been dispositioned
    // at all — that is precisely the "queued and never closed" state.
    let closure = disposition::closure_rate(&dispositions, &findings);
    let closure_by_source = disposition::closure_by_source(&dispositions, &findings);

    let confirmed = dispositions
        .iter()
        .filter(|d| d.verdict == DispositionVerdict::Confirmed)
        .count();
    let dismissed = dispositions
        .iter()
        .filter(|d| d.verdict == DispositionVerdict::Dismissed)
        .count();
    let false_positive = dispositions
        .iter()
        .filter(|d| d.verdict == DispositionVerdict::FalsePositive)
        .count();

    // Early warning for the "fix commit landed, nobody dispositioned it"
    // stale-backlog gap (2026-07-17 incident): findings a `reconcile-fixed`
    // run right now would confirm, recomputed read-only so it's visible even
    // when reconcile-fixed hasn't run this round.
    //
    // Tri-state (t3): a count that could not be computed is NOT zero. Zero
    // suppresses the warning line below, so folding the two would render a
    // broken store as "no stale finding".
    //
    // NOT COVERED BY ANY TEST, and this is why (third-party audit, t4): the
    // `Undetermined` arm below is UNREACHABLE from this call site short of a
    // TOCTOU race. `stale_undisposed_count` joins `scan_review_findings_all`
    // and `scan_dispositions` — the exact two readers this function already
    // resolved to `Known` above, on the same `cwd`; either being undetermined
    // would have returned `SomeUndetermined` before we got here. So there is no
    // pre-seeded store state that reaches it, and no test asserts
    // `STALE_UNDETERMINED_LINE` or the `null` stale count in a `--json` body
    // where the other keys are numbers. The branch is kept as the guard for a
    // store that changes MID-RUN (the ledgers are appended to concurrently by
    // other overwatch processes), which is the case a test here cannot stage
    // deterministically. Treat it as unverified, not as verified-by-obviousness.
    let stale_scan =
        reconcile::stale_undisposed_count(&cwd, ReconcileRange::LastN(STALE_SCAN_LAST_N));
    let stale_undisposed: Option<usize> = match &stale_scan {
        Determination::Known(n) => Some(*n),
        Determination::Undetermined(_) => {
            // `stale_undisposed_count` already announced which ledger failed.
            undetermined.push("stale-undisposed join");
            None
        }
    };
    let health = if undetermined.is_empty() {
        SourceHealth::AllRead
    } else {
        SourceHealth::SomeUndetermined
    };
    /// The line rendered in place of the stale-undisposed warning when the
    /// count could not be computed — never the silence a `0` produces.
    const STALE_UNDETERMINED_LINE: &str =
        "  WARNING: the stale-undisposed count could NOT be computed (a joined ledger \
         could not be read) — this is not a report of zero";

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "total": total,
                "false_positive_rate": fp_rate,
                "agreement_rate": agreement,
                "median_latency_secs": median_latency,
                "by_verdict": {
                    "confirmed": confirmed,
                    "dismissed": dismissed,
                    "false_positive": false_positive,
                },
                // `null` (never 0) when the join could not be computed; the
                // `undetermined_sources` list below names why.
                "stale_undisposed_with_fix_commit": stale_undisposed,
                "closure_rate": closure,
                "closure_by_source": closure_by_source
                    .iter()
                    .map(|(source, (closed, of))| {
                        (
                            source.clone(),
                            serde_json::json!({
                                "closed": closed,
                                "total": of,
                                "rate": if *of == 0 { None } else { Some(*closed as f64 / *of as f64) },
                            }),
                        )
                    })
                    .collect::<serde_json::Map<String, serde_json::Value>>(),
                // Always present (empty when every ledger was read), so a
                // consumer never has to interpret the key's absence.
                "undetermined_sources": undetermined,
            }))?
        );
        return Ok(health);
    }

    let fmt_closure = |closed: usize, of: usize| {
        if of == 0 {
            "n/a".to_string()
        } else {
            format!("{:.2} ({closed}/{of})", closed as f64 / of as f64)
        }
    };

    if total == 0 {
        println!("(no dispositions recorded yet — run overwatch record-disposition)");
        // Still report closure: "N findings queued, 0 closed" is a measurement,
        // and suppressing it here would make an entirely unclosed queue look the
        // same as an empty one.
        for (source, (closed, of)) in &closure_by_source {
            println!("  closure [{source}]: {}", fmt_closure(*closed, *of));
        }
        match stale_undisposed {
            Some(n) if n > 0 => println!(
                "  WARNING: {n} finding(s) have a landed fix commit but no disposition yet — run `overwatch reconcile-fixed`"
            ),
            Some(_) => {}
            None => println!("{STALE_UNDETERMINED_LINE}"),
        }
        return Ok(health);
    }

    let fmt_rate = |r: Option<f64>| {
        r.map(|v| format!("{:.2}", v))
            .unwrap_or_else(|| "n/a".to_string())
    };
    let fmt_secs = |v: Option<i64>| {
        v.map(|v| v.to_string())
            .unwrap_or_else(|| "n/a".to_string())
    };

    println!("Review-effectiveness report");
    println!("  total dispositions:     {total}");
    println!("  false-positive rate:    {}", fmt_rate(fp_rate));
    println!("  agreement rate:         {}", fmt_rate(agreement));
    println!("  median latency (secs):  {}", fmt_secs(median_latency));
    println!(
        "  by verdict: confirmed={confirmed} dismissed={dismissed} false_positive={false_positive}"
    );
    println!("  closure rate:           {}", fmt_rate(closure));
    for (source, (closed, of)) in &closure_by_source {
        println!("    [{source}]: {}", fmt_closure(*closed, *of));
    }
    match stale_undisposed {
        Some(n) if n > 0 => println!(
            "  WARNING: {n} finding(s) have a landed fix commit but no disposition yet — run `overwatch reconcile-fixed`"
        ),
        Some(_) => {}
        None => println!("{STALE_UNDETERMINED_LINE}"),
    }
    Ok(health)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_disposition() -> Disposition {
        Disposition::new(
            "finding-1".to_string(),
            DispositionVerdict::Confirmed,
            "tester".to_string(),
            42,
        )
    }

    /// TRUTHFULNESS: a contended HARD-SKIP persisted nothing, so the CLI must
    /// report `recorded:false` (with a contention reason) and a non-ok flag →
    /// nonzero exit — NOT the phantom `recorded:true` the old bare-`Ok(())`
    /// contention branch produced. RED before the fix (contention was
    /// indistinguishable from success and reported `recorded:true`).
    #[test]
    fn contended_disposition_is_reported_not_recorded() {
        let d = sample_disposition();
        let (line, ok) = disposition_result(AppendOutcome::SkippedContended, &d, d.verdict);
        assert!(
            !ok,
            "a contended skip must not report CLI success (nonzero exit)"
        );
        assert_eq!(
            line["recorded"], false,
            "contended skip must surface recorded:false, got {line}"
        );
        assert_eq!(
            line["reason"], "lock_contended",
            "contended skip must carry a contention reason, got {line}"
        );
    }

    /// A genuine persist / idempotent dedup is truthfully `recorded:true`,
    /// exit 0 — the fix must not regress the success surface.
    #[test]
    fn recorded_disposition_reports_success() {
        let d = sample_disposition();
        let (line, ok) = disposition_result(AppendOutcome::Recorded, &d, d.verdict);
        assert!(ok, "a persisted disposition must report CLI success");
        assert_eq!(line["recorded"], true);
        assert_eq!(line["finding_id"], "finding-1");
    }
}
