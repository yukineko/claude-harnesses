// テスト内の unwrap/expect は意図的な assert であって fail-open ではないので許可する。
// production 側は workspace の [workspace.lints.clippy] で deny のまま。
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
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

use harness_core::verdict::Determination;
use mutategate::{evaluate, parse_outcomes, GateOutcome, MutationSummary};

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

    /// Minimum acceptable kill-rate (killed / viable mutants). Must be in the
    /// half-open range `(KILL_RATE_EPSILON, 1.0]`: any value `<= 1e-9` (`0.0`,
    /// negative, or sub-epsilon) is REJECTED because it would disable the gate —
    /// a 0% kill-rate is bridged to a pass by the epsilon tolerance, so such a
    /// floor always passes. See `validate_min_kill_rate`.
    #[arg(long, default_value_t = DEFAULT_MIN_KILL_RATE)]
    min_kill_rate: f64,
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    if let Err(why) = mutategate::validate_min_kill_rate(cli.min_kill_rate) {
        eprintln!("mutategate: {why}");
        return ExitCode::from(2);
    }

    let json = match harness_core::boundary::read_to_string(&cli.outcomes) {
        Determination::Known(Some(j)) => j,
        Determination::Known(None) => {
            eprintln!(
                "mutategate: cannot read outcomes file {}: No such file or directory\n\
                 (run `cargo mutants` first, or point --outcomes at its outcomes.json)",
                cli.outcomes.display()
            );
            return ExitCode::from(2);
        }
        Determination::Undetermined(why) => {
            eprintln!(
                "mutategate: cannot read outcomes file {}: {why}\n\
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
    println!("{}", format_mutants_line(s));
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

    // Route the pass/fail decision through the shared verdict type
    // (`harness_core::verdict::Verdict`) rather than branching on the private
    // `passed` bool directly, so the exit code can never diverge from the
    // fail-closed contract every other migrated gate crate shares.
    let verdict = outcome.verdict();
    if verdict.blocks() {
        eprintln!("  FAIL: {}", outcome.reason);
        emit_violation(&outcome, &cli.outcomes);
        ExitCode::from(verdict.exit_code(1) as u8)
    } else {
        println!("  PASS: {}", outcome.reason);
        ExitCode::SUCCESS
    }
}

/// Render the one-line `mutants:` breakdown printed for every run. The viable
/// breakdown must enumerate **every** state that `MutationSummary::viable()`
/// counts — `caught + timeout + missed + unknown` — so the parts always sum to
/// the printed viable count. Omitting `unknown` (as an earlier version did) made
/// the breakdown under-sum whenever a mutant carried an unrecognised summary
/// state (CA-mutategate-04). Pure/testable: no I/O.
fn format_mutants_line(s: &MutationSummary) -> String {
    format!(
        "  mutants: {} viable ({} caught, {} timeout, {} missed, {} unknown) + {} unviable",
        s.viable(),
        s.caught,
        s.timeout,
        s.missed,
        s.unknown,
        s.unviable,
    )
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

/// Record a fleet-level violation for a FAILed gate, fail-soft: never changes
/// the gate's exit code or stdout, and never panics when the overwatch store is
/// unwritable (e.g. sandboxed/read-only HOME, missing repo root). A PASS never
/// calls this, so a PASS emits nothing.
///
/// Fail-soft means the EXIT CODE is unaffected. It does NOT mean the failure is
/// invisible, and that distinction is the whole point of this wrapper. The
/// previous version swallowed both failure paths outright — `Err(_) => return`
/// on the cwd lookup and `let _ = append_violation(..)` on the write — with the
/// comment "any store I/O failure is swallowed, not surfaced". Per CLAUDE.md §1
/// a module only earns the no-judgement carve-out when it has no downstream
/// consumer, or when the gap shows up as an explicit `unknown`. This one has a
/// consumer and showed nothing:
///
/// ```text
/// append_violation -> violation ledger -> scan_violations
///   -> detect_recurrence -> filter(is_systemic)
///     -> review_queue::build_queue -> bridge -> backlog add (p0)
/// ```
///
/// A dropped write lowers the recurrence count, so a genuinely systemic failure
/// can stay under the threshold and never reach the queue. The READ side is
/// already careful about this — `bridge.rs` refuses to report an unreadable
/// ledger as "zero systemic violations" — but no amount of care on the read side
/// can recover an event that was never written. The reader sees a perfectly
/// readable ledger and correctly reports zero. That is the mirror gap (§6): the
/// fix landed on the reader and not on the writer.
///
/// So the write still cannot fail the gate, but it can no longer fail in
/// silence: [`emit_violation_at`] returns why, and this wrapper prints it next
/// to the FAIL line the gate is already emitting.
fn emit_violation(outcome: &GateOutcome, outcomes_path: &Path) {
    let cwd = match std::env::current_dir() {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "  (note: this FAIL was NOT recorded to the fleet violation ledger — \
                 the working directory could not be resolved: {e}. The gate's verdict \
                 above stands; only the cross-run recurrence signal was lost.)"
            );
            return;
        }
    };
    if let Err(why) = emit_violation_at(&cwd, outcome, outcomes_path) {
        eprintln!(
            "  (note: this FAIL was NOT recorded to the fleet violation ledger — {why}. \
             The gate's verdict above stands; only the cross-run recurrence signal was lost.)"
        );
    }
}

/// The testable core of [`emit_violation`], with the working directory injected
/// rather than read from the process, and the store failure RETURNED rather than
/// dropped. Returns `Err(reason)` when the event could not be persisted.
fn emit_violation_at(
    cwd: &Path,
    outcome: &GateOutcome,
    outcomes_path: &Path,
) -> Result<(), String> {
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

    // `build_event` returning None is not a failure: it means this event has no
    // bucketable signature, so there is deliberately nothing to record.
    match event {
        Some(event) => overwatch::store::append_violation(cwd, &event)
            .map_err(|e| format!("the overwatch violation store could not be written: {e}")),
        None => Ok(()),
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

    // ── The violation write must not fail in SILENCE (audit finding F-1/F-2,
    //    docs/audit-mutategate-verdict-paths.md §3). The gate's exit code is
    //    deliberately unaffected; what changed is that a lost record is now
    //    reported instead of dropped.
    //
    //    HOME is process-global, so these two tests serialise on their own lock
    //    and always restore it. No other test in this crate reads HOME. ──────
    static HOME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Run `f` with `$HOME` pointed at `home`, restoring the previous value
    /// afterwards even if `f` panics.
    fn with_home<T>(home: &Path, f: impl FnOnce() -> T) -> T {
        let guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var_os("HOME");
        std::env::set_var("HOME", home);
        let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        match prev {
            Some(p) => std::env::set_var("HOME", p),
            None => std::env::remove_var("HOME"),
        }
        drop(guard);
        match out {
            Ok(v) => v,
            Err(p) => std::panic::resume_unwind(p),
        }
    }

    #[test]
    fn an_unwritable_store_is_reported_not_swallowed() {
        let tmp = tempfile::tempdir().unwrap();
        // HOME points at a FILE, so the store's `create_dir_all` cannot succeed.
        let home_file = tmp.path().join("home-is-a-file");
        std::fs::write(&home_file, b"not a directory").unwrap();

        let err = with_home(&home_file, || {
            emit_violation_at(
                tmp.path(),
                &outcome(Some(0.5)),
                Path::new("mutants.out/o.json"),
            )
        })
        .expect_err("an unwritable store must be reported, not swallowed");

        assert!(
            err.contains("could not be written"),
            "the reason must say what was lost, got: {err}"
        );
    }

    /// ANTI-VACUITY CONTROL. Without this, an `emit_violation_at` that returned
    /// `Err` unconditionally — or that never wrote anything at all — would
    /// satisfy the test above while recording nothing, ever. A writable store
    /// must still return `Ok`.
    #[test]
    fn a_writable_store_still_records_and_returns_ok() {
        let tmp = tempfile::tempdir().unwrap();
        let home_dir = tmp.path().join("home");
        std::fs::create_dir_all(&home_dir).unwrap();

        // The ledger path is resolved INSIDE the sandbox: `violations_path`
        // reads $HOME, so computing it after `with_home` returns would point at
        // the developer's real home and assert against a file this test never
        // wrote (which is exactly how this test first failed).
        let (res, ledger) = with_home(&home_dir, || {
            let res = emit_violation_at(
                tmp.path(),
                &outcome(Some(0.5)),
                Path::new("mutants.out/o.json"),
            );
            let ledger = overwatch::store::violations_path(tmp.path()).unwrap();
            (res, ledger)
        });

        assert!(res.is_ok(), "a writable store must succeed, got: {res:?}");
        // And it must actually have written something — an `Ok` that persisted
        // nothing is the same silence in a different costume.
        assert!(
            ledger.exists(),
            "expected a violation ledger at {}",
            ledger.display()
        );
        assert!(
            ledger.starts_with(&home_dir),
            "the test wrote outside its sandbox: {}",
            ledger.display()
        );
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

    // ── CA-mutategate-04: the printed viable breakdown must account for
    //    `unknown`, so caught + timeout + missed + unknown sums to the viable
    //    count. Omitting unknown made the breakdown under-sum whenever any
    //    mutant carried an unrecognised summary state. ──────────────────────
    #[test]
    fn mutants_line_breakdown_accounts_for_unknown() {
        let s = MutationSummary {
            caught: 3,
            missed: 2,
            timeout: 1,
            unviable: 4,
            unknown: 5,
            ..Default::default()
        };
        // viable = caught(3) + timeout(1) + missed(2) + unknown(5) = 11.
        assert_eq!(s.viable(), 11);
        let line = format_mutants_line(&s);
        assert!(
            line.contains("11 viable"),
            "line should report the viable count: {line}"
        );
        assert!(
            line.contains("5 unknown"),
            "the viable breakdown omits `unknown`, so caught+timeout+missed does \
             not sum to the viable count: {line}"
        );
    }
}
