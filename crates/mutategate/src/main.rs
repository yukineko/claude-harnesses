//! mutategate CLI — read a `cargo-mutants` `outcomes.json`, compute the kill-rate
//! of the existing tests, and exit non-zero when it falls below a threshold.
//!
//! This binary does **not** run the mutation engine itself; it consumes the JSON
//! that `cargo mutants` leaves in `mutants.out/outcomes.json`. Wiring the engine
//! run + this gate together is the job of `scripts/mutation-gate.sh` and the
//! `.github/workflows/mutation.yml` CI job. Keeping the scoring here (pure,
//! unit-tested) makes the pass/fail decision deterministic and independent of the
//! slow engine.
//!
//! Exit codes:
//!   * `0`  — kill-rate met the threshold (gate passed).
//!   * `1`  — kill-rate below threshold, or no viable mutants (gate failed).
//!   * `2`  — usage/IO/parse error (could not evaluate the gate at all).

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;

use mutategate::{evaluate, parse_outcomes, GateOutcome};

/// Default minimum kill-rate. 0.80 mirrors the practical robustness bar used by
/// established mutation tools (e.g. PIT) and the Meta ACH line of work: below it,
/// a suite is demonstrably missing detectable faults. It is intentionally
/// conservative for the pilot so the gate is signal, not noise; raise it as the
/// pilot crate's suite hardens.
const DEFAULT_MIN_KILL_RATE: f64 = 0.80;

/// Default location `cargo-mutants` writes its machine-readable results to.
const DEFAULT_OUTCOMES: &str = "mutants.out/outcomes.json";

#[derive(Parser, Debug)]
#[command(
    name = "mutategate",
    about = "Fail (exit 1) when the cargo-mutants kill-rate of the existing tests is below a threshold."
)]
struct Cli {
    /// Path to the cargo-mutants `outcomes.json`.
    #[arg(long, default_value = DEFAULT_OUTCOMES)]
    outcomes: PathBuf,

    /// Minimum acceptable kill-rate (killed / viable mutants), 0.0..=1.0.
    #[arg(long, default_value_t = DEFAULT_MIN_KILL_RATE)]
    min_kill_rate: f64,
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    if !(0.0..=1.0).contains(&cli.min_kill_rate) {
        eprintln!(
            "mutategate: --min-kill-rate must be within 0.0..=1.0 (got {})",
            cli.min_kill_rate
        );
        return ExitCode::from(2);
    }

    let json = match std::fs::read_to_string(&cli.outcomes) {
        Ok(j) => j,
        Err(e) => {
            eprintln!(
                "mutategate: cannot read outcomes file {}: {e}\n\
                 (run `cargo mutants` first, or point --outcomes at its outcomes.json)",
                cli.outcomes.display()
            );
            return ExitCode::from(2);
        }
    };

    let summary = match parse_outcomes(&json) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "mutategate: failed to parse {}: {e}",
                cli.outcomes.display()
            );
            return ExitCode::from(2);
        }
    };

    let outcome = evaluate(summary, cli.min_kill_rate);
    let s = &outcome.summary;

    println!("mutategate: mutation kill-rate gate");
    println!(
        "  mutants: {} viable ({} caught, {} timeout, {} missed) + {} unviable",
        s.viable(),
        s.caught,
        s.timeout,
        s.missed,
        s.unviable,
    );
    match outcome.kill_rate {
        Some(kr) => println!(
            "  kill-rate: {:.1}%   threshold: {:.1}%",
            kr * 100.0,
            outcome.threshold * 100.0
        ),
        None => println!(
            "  kill-rate: n/a       threshold: {:.1}%",
            outcome.threshold * 100.0
        ),
    }

    if outcome.passed {
        println!("  PASS: {}", outcome.reason);
        ExitCode::SUCCESS
    } else {
        eprintln!("  FAIL: {}", outcome.reason);
        emit_violation(&outcome, &cli.outcomes);
        ExitCode::from(1)
    }
}

/// Deterministic, non-empty reason-class token identifying *why* the gate
/// failed. `MutationSummary` carries only aggregate counts (no per-mutant
/// operator), so unlike blastguard/propguard the discriminator here cannot
/// be a specific rule/property id — it is the class of failure reason
/// instead. Kept in sync with the two `passed: false` arms of
/// [`mutategate::evaluate`].
fn failure_reason_class(outcome: &GateOutcome) -> &'static str {
    if outcome.kill_rate.is_none() {
        "no-viable-mutants"
    } else {
        "below-threshold"
    }
}

/// Record a fleet-level violation for a FAILed gate, fail-soft: never
/// changes the gate's exit code or stdout/stderr, and never panics when the
/// overwatch store is unwritable (e.g. sandboxed/read-only HOME, missing
/// repo root). A PASS never calls this, so a PASS emits nothing.
fn emit_violation(outcome: &GateOutcome, outcomes_path: &Path) {
    let cwd = match std::env::current_dir() {
        Ok(c) => c,
        Err(_) => return,
    };

    let discriminator = failure_reason_class(outcome);
    let raw = overwatch::violation::RawViolation {
        mutation_operator: Some(discriminator),
        ..Default::default()
    };

    let session_id = std::env::var("CLAUDE_CODE_SESSION_ID")
        .unwrap_or_else(|_| format!("pid-{}", std::process::id()));
    let task_key = outcomes_path.display().to_string();
    let now = overwatch::store::now();

    let event = overwatch::violation::build_event(
        overwatch::violation::ViolationSource::Mutategate,
        &raw,
        task_key,
        session_id,
        now,
        Some(outcome.reason.clone()),
    );

    // Best-effort: any store I/O failure is swallowed, not surfaced.
    if let Some(event) = event {
        let _ = overwatch::store::append_violation(&cwd, &event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(kill_rate: Option<f64>) -> GateOutcome {
        GateOutcome {
            summary: mutategate::MutationSummary::default(),
            kill_rate,
            threshold: 0.80,
            passed: false,
            reason: "test".to_string(),
        }
    }

    #[test]
    fn failure_reason_class_is_nonempty_and_deterministic() {
        assert_eq!(failure_reason_class(&outcome(None)), "no-viable-mutants");
        assert_eq!(failure_reason_class(&outcome(Some(0.5))), "below-threshold");
        // Repeated calls on equivalent input must agree (deterministic).
        assert_eq!(
            failure_reason_class(&outcome(Some(0.5))),
            failure_reason_class(&outcome(Some(0.5)))
        );
    }
}
