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
use crate::disposition::{self, Disposition, DispositionVerdict};
use crate::store;
use anyhow::Result;

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
        Ok(()) => {
            println!(
                "{}",
                serde_json::json!({
                    "recorded": true,
                    "finding_id": record.finding_id,
                    "verdict": verdict.label(),
                })
            );
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

/// Read the disposition ledger (joined against the review-findings store) and
/// print the review-effectiveness report: false-positive rate, agreement
/// rate, median latency (seconds), and a per-verdict breakdown. Fail-soft: a
/// missing/empty store yields a zero/`None` report rather than an error.
pub fn metrics(json: bool) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let dispositions = store::read_dispositions(&cwd).unwrap_or_default();
    let findings = store::read_review_findings(&cwd).unwrap_or_default();

    let total = dispositions.len();
    let fp_rate = disposition::false_positive_rate(&dispositions);
    let agreement = disposition::agreement_rate(&dispositions);
    let median_latency = disposition::median_latency_secs(&dispositions, &findings);

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
            }))?
        );
        return Ok(());
    }

    if total == 0 {
        println!("(no dispositions recorded yet — run overwatch record-disposition)");
        return Ok(());
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
    Ok(())
}
