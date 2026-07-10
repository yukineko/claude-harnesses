/// CLI-facing glue for the Continuous-Audit round metrics ledger (2630b4c5).
///
/// `record` appends one round record to the audit-round ledger (the deterministic
/// counterpart of the LLM-driven finder/verifier step); `metrics` reads the
/// ledger back and prints the convergence report (per-round new-findings trend,
/// closure-rate, and a `converging` flag) either human-readable or as JSON.
///
/// Both paths are **fail-soft**: `scripts/continuous-audit.sh` calls `record`
/// while orchestrating a round, and a store-write failure must NEVER break the
/// audit loop — an unwritable ledger is reported to stderr rather than
/// propagated, matching overwatch's observational / never-break-a-turn invariant.
use crate::audit_round::{self, AuditRound, DEFAULT_CONVERGENCE_WINDOW};
use crate::store;
use anyhow::Result;

/// Record one Continuous-Audit round's metrics. Called by
/// `scripts/continuous-audit.sh` after a finder→verifier review round completes.
///
/// `target` is a comma/space-separated crate list (normalized internally).
/// Fail-soft: a store write error is reported to stderr but returns `Ok(())` so
/// the caller (the audit loop) is never broken by logging.
pub fn record(
    round: String,
    target: &str,
    new_findings: u64,
    confirmed: u64,
    regression_tests_added: u64,
) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let now = store::now();
    let targets = audit_round::parse_targets(target);
    let record = AuditRound::new(
        round,
        &targets,
        new_findings,
        confirmed,
        regression_tests_added,
        now,
    );

    match store::append_audit_round(&cwd, &record) {
        Ok(()) => {
            println!(
                "{}",
                serde_json::json!({
                    "recorded": true,
                    "round": record.round,
                    "targets": record.targets,
                    "new_findings": new_findings,
                    "confirmed": confirmed,
                    "regression_tests_added": regression_tests_added,
                })
            );
        }
        Err(e) => {
            eprintln!("overwatch: WARNING could not record audit round (continuing): {e}");
            println!(
                "{}",
                serde_json::json!({ "recorded": false, "reason": "store-write-failed" })
            );
        }
    }
    Ok(())
}

/// Read the audit-round ledger and print convergence metrics. `window` bounds
/// how many trailing rounds the `converging` check considers (default
/// [`DEFAULT_CONVERGENCE_WINDOW`]). Fail-soft: a missing/empty ledger yields a
/// zero-round report rather than an error.
pub fn metrics(json: bool, window: Option<usize>) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let rounds = store::read_audit_rounds(&cwd).unwrap_or_default();
    let window = window.unwrap_or(DEFAULT_CONVERGENCE_WINDOW);
    let report = audit_round::compute_metrics(&rounds, window);

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    if report.rounds.is_empty() {
        println!("(no audit rounds recorded yet — run scripts/continuous-audit.sh)");
        return Ok(());
    }

    println!("Continuous-Audit convergence report");
    println!("  rounds recorded: {}", report.rounds.len());
    println!("  per-round new-findings (decreasing => converging):");
    for r in &report.rounds {
        let cr = r
            .closure_rate
            .map(|v| format!("{:.2}", v))
            .unwrap_or_else(|| "n/a".to_string());
        println!(
            "    round {:>3}: new={:<4} confirmed={:<4} tests={:<4} closure={}",
            r.round, r.new_findings, r.confirmed, r.regression_tests_added, cr
        );
    }
    println!("  total new findings:       {}", report.total_new_findings);
    println!(
        "  cumulative confirmed:     {}",
        report.cumulative_confirmed
    );
    println!(
        "  cumulative reg. tests:    {}",
        report.cumulative_regression_tests_added
    );
    let overall = report
        .closure_rate
        .map(|v| format!("{:.2}", v))
        .unwrap_or_else(|| "n/a".to_string());
    println!("  overall closure-rate:     {overall}");
    println!(
        "  converging (last {} rounds): {}",
        report.convergence_window, report.converging
    );
    Ok(())
}
