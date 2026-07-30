/// CLI-facing glue for the Continuous-Audit round metrics ledger (2630b4c5).
///
/// `record` appends one round record to the audit-round ledger (the deterministic
/// counterpart of the LLM-driven finder/verifier step); `metrics` reads the
/// ledger back and prints the convergence report (per-round new-findings trend,
/// closure-rate, and a tri-state `converging` flag) either human-readable or as
/// JSON.
///
/// READING the ledger is fail-CLOSED, and deliberately so: `metrics` and `close`
/// both abort when the ledger cannot be read or parsed, rather than proceeding
/// from `unwrap_or_default()`'s empty vec. Reporting metrics over a history you
/// could not read produced a *better* verdict than the true one, and `close`
/// rewrites the whole file, so a partial read there would delete records. See
/// `store::read_audit_rounds` and `tests/verdict_monotonicity.rs`.
///
/// Reporting failures here are **fail-soft**: a store-write failure is reported
/// to stderr rather than propagated, matching overwatch's observational
/// invariant — an unwritable ledger should not destroy a round's work.
///
/// The round ACCEPTANCE check is not, and must not be described as if it were.
/// `record` refuses outright (non-zero, nothing written) when a round confirms
/// findings it does not close; see [`audit_round::closure_incomplete`]. The
/// earlier version of this header said "both paths are fail-soft ... must NEVER
/// break the audit loop", and that sentence is precisely the shape CLAUDE.md §1
/// forbids around a verdict: it reads as blanket permission to map any bad
/// outcome to success. The narrower claim — reporting failures degrade, verdicts
/// do not — is the one that is true.
use crate::audit_round::{self, AuditRound, DEFAULT_CONVERGENCE_WINDOW};
use crate::lock::LeaseLock;
use crate::review_finding::ReviewFinding;
use crate::store;
use anyhow::Result;
use std::path::Path;

/// Record one Continuous-Audit round's metrics. Called by
/// `scripts/continuous-audit.sh` after a finder→verifier review round completes.
///
/// `target` is a comma/space-separated crate list (normalized internally).
/// Fail-soft: a store write error is reported to stderr but returns `Ok(())` so
/// the caller (the audit loop) is never broken by logging.
///
/// `finder_model` / `verifier_model` are optional. When BOTH are supplied and
/// denote the SAME model (canonical compare), the Continuous-Audit
/// `finder != verifier` MUST is violated (generation and verification share a
/// blind spot). This is enforced DETERMINISTICALLY here rather than left to
/// SKILL.md prose: a high-severity warning finding (deterministic, round-derived
/// id) is recorded into the review queue and a stderr warning is emitted. It is
/// **never** a hard failure — the audit loop is never broken (never-break-a-turn),
/// so a warning finding + the round record are preferred over aborting. When one
/// or both model args are omitted, the check is skipped entirely (backward
/// compatible: existing callers that pass no model args behave exactly as before).
///
/// `unverified` is the round's UNDETERMINED count (verdicts that were neither
/// confirmed nor refuted). It is stored alongside `confirmed` — never folded
/// into it — so the ledger cannot be read as if every finding was settled.
#[allow(clippy::too_many_arguments)]
pub fn record(
    round: String,
    target: &str,
    new_findings: u64,
    confirmed: u64,
    unverified: u64,
    regression_tests_added: u64,
    finder_model: Option<&str>,
    verifier_model: Option<&str>,
) -> Result<()> {
    let cwd = std::env::current_dir()?;

    // ACCEPTANCE CONDITION, checked before anything is written (5b33b4cd).
    //
    // A round that confirms findings without closing them is refused outright:
    // no ledger append, no findings ingest, non-zero exit. Recording it would
    // put a round in the convergence ledger that proves the audit found a
    // defect class and left it live, which is what `converging:false` had been
    // reporting for six rounds running.
    //
    // This path is deliberately NOT fail-soft, and that is a departure from the
    // rest of this module. The module header calls every path fail-soft so the
    // audit loop is never broken; CLAUDE.md §1 says that reasoning must not be
    // applied to code that holds a verdict, and this is one. Refusing a round is
    // not "breaking the loop" — it is the loop working.
    if audit_round::closure_incomplete(confirmed, regression_tests_added) {
        anyhow::bail!(
            "refusing to record round {round:?}: {confirmed} finding(s) confirmed but only \
             {regression_tests_added} closed by regression tests. Every CONFIRMED finding must \
             be pinned by a regression test in the SAME round — otherwise the next round \
             re-harvests the same defect class and the audit never converges. Write the \
             missing {} test(s), then re-record. Do not lower --confirmed to match.",
            confirmed.saturating_sub(regression_tests_added),
        );
    }

    let now = store::now();
    let targets = audit_round::parse_targets(target);
    let record = AuditRound::new(
        round,
        &targets,
        new_findings,
        confirmed,
        regression_tests_added,
        now,
    )
    .with_unverified(unverified);

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
                    "unverified": unverified,
                    // Report the STORED (clamped-to-confirmed) value, not the
                    // raw CLI arg, so the printed output never claims a
                    // regression_tests_added > confirmed (CA-overwatch-004).
                    "regression_tests_added": record.regression_tests_added,
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

    // Deterministic finder != verifier model-diversity enforcement. Runs AFTER
    // (and independently of) the round append so it fires even if the ledger
    // write failed. Only checks when BOTH models were supplied (backward compat).
    enforce_model_diversity(&cwd, &record.round, finder_model, verifier_model, now);

    Ok(())
}

/// Enforce the `finder != verifier` MUST for a recorded round. When both models
/// are present and canonically equal, record a high-severity warning finding
/// (round-derived id, idempotent) into the review queue and warn on stderr.
/// Fail-soft and non-fatal: a store-write failure is only logged; the audit loop
/// is never broken.
fn enforce_model_diversity(
    cwd: &std::path::Path,
    round: &str,
    finder_model: Option<&str>,
    verifier_model: Option<&str>,
    now: i64,
) {
    let Some(finding) = model_collision_finding(round, finder_model, verifier_model, now) else {
        return;
    };
    // Safe: a finding is built ONLY when both models are present & non-empty.
    let finder = finder_model.unwrap_or_default().trim();
    eprintln!(
        "overwatch: WARNING finder と verifier が同一モデル ({finder}): MUST 違反 — \
         review-queue に警告 finding ({finding_id}) を記録 (loop は継続 / fail-soft)",
        finding_id = finding.finding_id
    );
    if let Err(e) = store::append_review_finding(cwd, &finding) {
        eprintln!("overwatch: WARNING could not record model-collision finding (continuing): {e}");
    }
}

/// Pure decision + builder for the finder==verifier model-collision finding:
/// return `Some(finding)` iff [`audit_round::model_diversity_violation`] holds
/// (both models present, non-empty, canonically equal), else `None`. Isolating
/// the decision from the store side-effect makes the "same-model round is
/// surfaced, distinct/missing-model round is NOT" contract directly testable
/// without touching the review-findings ledger. The finding carries a
/// round-derived (idempotent) id and `high` severity.
fn model_collision_finding(
    round: &str,
    finder_model: Option<&str>,
    verifier_model: Option<&str>,
    now: i64,
) -> Option<ReviewFinding> {
    if !audit_round::model_diversity_violation(finder_model, verifier_model) {
        return None;
    }
    let finder = finder_model.unwrap_or_default().trim();
    let verifier = verifier_model.unwrap_or_default().trim();
    let finding_id = audit_round::model_collision_finding_id(round);
    let summary = format!(
        "finder と verifier が同一モデル: MUST 違反 (finder={finder}, verifier={verifier}) — \
         model diversity 要件 (生成と検証の盲点共有を防ぐ) を満たさない"
    );
    Some(ReviewFinding::new(
        finding_id,
        "continuous-audit".to_string(),
        Some("high".to_string()),
        summary,
        None,
        None,
        now,
    ))
}

/// Close a Continuous-Audit round: SET its `regression_tests_added` to `tests`
/// (the fix-side feedback the finder-time [`record`] cannot know, since the
/// regression tests don't exist when a round is first recorded). Read-modify-write
/// on the ledger: read all rounds, update the one matching `round` in memory via
/// [`audit_round::set_round_tests`], write it back. After this, `audit-metrics`
/// reports the round's honest `closure_rate` and the cumulative `converging`
/// signal reflects the fixes actually landed.
///
/// Fail-soft (never-break-a-turn): an unknown round-id, or a store error, is
/// reported (JSON on stdout + stderr note) but never panics. Idempotent: SETting
/// the same `tests` twice is a no-op, so re-running a backfill does not
/// double-count.
pub fn close(round: String, tests: u64) -> Result<()> {
    let cwd = std::env::current_dir()?;
    match close_at(&cwd, &round, tests) {
        Ok(CloseOutcome::NotFound) => {
            eprintln!(
                "overwatch: WARNING audit-round close: no round matching id {round:?} (ledger unchanged)"
            );
            println!(
                "{}",
                serde_json::json!({ "closed": false, "reason": "round-not-found", "round": round })
            );
        }
        Ok(CloseOutcome::Closed) => {
            println!(
                "{}",
                serde_json::json!({
                    "closed": true,
                    "round": round,
                    "regression_tests_added": tests,
                })
            );
        }
        Err(e) => {
            eprintln!("overwatch: WARNING could not rewrite audit rounds (continuing): {e}");
            println!(
                "{}",
                serde_json::json!({ "closed": false, "reason": "store-write-failed", "round": round })
            );
        }
    }
    Ok(())
}

/// Result of the locked read-modify-write in [`close_at`], so [`close`] can
/// render the right message and the concurrency test can assert on the outcome
/// without parsing stdout.
enum CloseOutcome {
    /// The round was found and its `regression_tests_added` was rewritten.
    Closed,
    /// No round matched the id; the ledger was left unchanged.
    NotFound,
}

/// Locked read-modify-write core of [`close`], parameterized on an explicit
/// `cwd` so the concurrency regression test can drive it against a temp store
/// without mutating the process cwd.
///
/// Holds the store [`LeaseLock`] across the WHOLE read->modify->rewrite. Without
/// it, a round appended (by a lock-respecting writer, e.g. a concurrent
/// `audit-round record`) between this read and its `rewrite_audit_rounds` would
/// be clobbered by the rewrite of the stale snapshot. Fail-soft: LeaseLock
/// degrades to unlocked on timeout and never panics; the round is still recorded
/// and the audit loop is never broken.
fn close_at(cwd: &Path, round: &str, tests: u64) -> Result<CloseOutcome> {
    // HARD-SKIP on contention: skip (report NotFound → "left unchanged") rather
    // than proceed to an unlocked read->modify->rewrite that could clobber a
    // concurrently-appended round. The audit loop is never broken; the close
    // re-runs later. The old fail-soft `acquire` left that window open.
    let _lock = match LeaseLock::acquire_or_skip(cwd) {
        Some(l) => l,
        None => return Ok(CloseOutcome::NotFound),
    };
    // Do NOT fall back to an empty ledger here. This is a read->modify->rewrite:
    // rewriting from a ledger we could not fully read would DESTROY the rounds
    // we failed to parse. `unwrap_or_default()` used to make that the quiet
    // default path.
    let rounds = match store::read_audit_rounds(cwd)?.require() {
        Ok(r) => r,
        Err(v) => {
            anyhow::bail!(
                "refusing to close round {round:?}: {}. Closing rewrites the \
                 whole ledger, so proceeding from a partial read would drop \
                 every record that failed to parse.",
                v.reason()
                    .map(|r| r.as_str())
                    .unwrap_or("ledger undetermined")
            )
        }
    };
    let (updated, found) = audit_round::set_round_tests(&rounds, round, tests);
    if !found {
        return Ok(CloseOutcome::NotFound);
    }
    // Test-only race widener (no-op in prod): widens the window between the read
    // above and the rewrite below, held UNDER the lock so a lock-respecting
    // concurrent writer is serialized against it.
    artificial_close_delay();
    store::rewrite_audit_rounds(cwd, &updated)?;
    Ok(CloseOutcome::Closed)
}

/// Sleep for `OVERWATCH_TEST_CLOSE_DELAY_MS` ms if set (a no-op otherwise), to
/// widen [`close_at`]'s locked read->rewrite window for the concurrency
/// regression test. Never set in production. Mirrors `lease::artificial_delay`.
fn artificial_close_delay() {
    if let Some(ms) = std::env::var("OVERWATCH_TEST_CLOSE_DELAY_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
    {
        std::thread::sleep(std::time::Duration::from_millis(ms));
    }
}

/// Read the audit-round ledger and print convergence metrics. `window` bounds
/// how many trailing rounds the `converging` check considers (default
/// [`DEFAULT_CONVERGENCE_WINDOW`]). Fail-soft: a missing/empty ledger yields a
/// zero-round report rather than an error.
pub fn metrics(json: bool, window: Option<usize>) -> Result<()> {
    let cwd = std::env::current_dir()?;
    // A ledger that cannot be read is not a ledger with no rounds. Reporting
    // metrics over `unwrap_or_default()`'s empty vec is how `converging: true`
    // became reachable by damaging the file.
    let rounds = match store::read_audit_rounds(&cwd)?.require() {
        Ok(r) => r,
        Err(v) => {
            anyhow::bail!(
                "cannot report audit metrics: {}",
                v.reason()
                    .map(|r| r.as_str())
                    .unwrap_or("ledger undetermined")
            )
        }
    };
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
            "    round {:>3}: new={:<4} confirmed={:<4} unverified={:<4} tests={:<4} closure={}",
            r.round, r.new_findings, r.confirmed, r.unverified, r.regression_tests_added, cr
        );
    }
    println!("  total new findings:       {}", report.total_new_findings);
    println!(
        "  cumulative confirmed:     {}",
        report.cumulative_confirmed
    );
    println!(
        "  cumulative unverified:    {} (undetermined — still open)",
        report.cumulative_unverified
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
    // Print the undetermined arm as words, not as `None`. A reader skimming this
    // must not be able to mistake "cannot tell yet" for a verdict either way —
    // that confusion is the whole defect this tri-state removes.
    let converging = match report.converging {
        Some(true) => "yes".to_string(),
        Some(false) => "NO".to_string(),
        None => format!(
            "unknown (only {} round(s) in scope — a trend needs 2)",
            report.rounds.len().min(report.convergence_window.max(1))
        ),
    };
    println!(
        "  converging (last {} rounds): {converging}",
        report.convergence_window
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_surfaces_same_model_round_as_high_severity_finding() {
        // A same-model round (finder == verifier) must NOT pass silently: the
        // CLI decision builds a high-severity, round-derived finding.
        let finding = model_collision_finding(
            "2026W28",
            Some("claude-opus-4-8"),
            Some("claude-opus-4-8"),
            42,
        )
        .expect("same-model round must surface a model-collision finding");
        assert_eq!(
            finding.finding_id,
            audit_round::model_collision_finding_id("2026W28")
        );
        assert_eq!(finding.severity.as_deref(), Some("high"));
        assert_eq!(finding.source, "continuous-audit");
        assert!(
            finding.summary.contains("MUST"),
            "summary must name the MUST violation: {}",
            finding.summary
        );
        assert_eq!(finding.ts, 42);

        // Case / whitespace variants of the same model still surface.
        assert!(
            model_collision_finding("r", Some("  Opus "), Some("opus"), 1).is_some(),
            "case/whitespace-different spellings of the same model must surface"
        );
    }

    #[test]
    fn cli_does_not_surface_distinct_or_missing_models() {
        // Distinct models satisfy the diversity MUST => no finding.
        assert!(model_collision_finding(
            "r",
            Some("claude-3-5-sonnet"),
            Some("claude-3-5-opus"),
            1
        )
        .is_none());
        // A missing model field (None on either side) => no finding (backward
        // compatible with callers that pass no model args).
        assert!(model_collision_finding("r", None, Some("opus"), 1).is_none());
        assert!(model_collision_finding("r", Some("opus"), None, 1).is_none());
        assert!(model_collision_finding("r", None, None, 1).is_none());
        // Empty / whitespace-only fields are "missing" => no FALSE finding.
        assert!(model_collision_finding("r", Some(""), Some(""), 1).is_none());
        assert!(model_collision_finding("r", Some("   "), Some("  "), 1).is_none());
    }

    // AXIS 1: a round appended concurrently with an `audit-round close`'s
    // read-modify-write must NOT be lost. `close_at` now holds the store
    // LeaseLock across its whole read->rewrite, so a lock-respecting concurrent
    // writer (the appender below) serializes against it instead of having its
    // append clobbered by the rewrite of a stale snapshot.
    //
    // RED (before the fix): remove the `LeaseLock::acquire` in `close_at` and
    // this fails — the appended `r2` is silently dropped by close's rewrite.
    // GREEN: with the lock, both the closed `r1` (tests=7) and `r2` survive.
    /// The IO arm of the read tri-state, which the pure monotonicity property
    /// in `tests/verdict_monotonicity.rs` cannot reach (it drives text, not
    /// files) — so it is pinned here, serialized on `HOME_ENV_LOCK`.
    ///
    /// `Degradation::Unreadable`: the ledger exists and is full of rounds, but
    /// the process cannot open it. That MUST read as undetermined. It used to be
    /// `Err(_) => Ok(Vec::new())`, i.e. "there is no audit history", which is
    /// how `chmod 000` on this file flipped the shipped 0.2.15 binary's report
    /// from `converging: false` to `converging: true` at exit 0.
    ///
    /// RED (before the fix): restore that arm and this returns `Known([])`.
    #[test]
    #[cfg(unix)]
    fn an_unreadable_ledger_is_undetermined_not_an_empty_history() {
        use harness_core::verdict::Determination;
        use std::os::unix::fs::PermissionsExt;

        let _guard = store::HOME_ENV_LOCK.lock().unwrap();
        let prev_home = std::env::var_os("HOME");
        let dir = std::env::temp_dir().join(format!(
            "overwatch-audit-unreadable-{}-{}",
            std::process::id(),
            store::now()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("HOME", &dir);

        // A real, non-empty, perfectly valid ledger...
        let now = store::now();
        for (i, n) in [5u64, 9].iter().enumerate() {
            let r = AuditRound::new(
                format!("r{i}"),
                &["overwatch".to_string()],
                *n,
                0,
                0,
                now + i as i64,
            );
            store::append_audit_round(&dir, &r).unwrap();
        }
        let path = store::audit_rounds_path(&dir).unwrap();
        let determined = store::read_audit_rounds(&dir).unwrap();
        assert!(
            matches!(&determined, Determination::Known(r) if r.len() == 2),
            "control: the ledger must read cleanly before we take it away"
        );

        // ...that the process can no longer open.
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o000);
        std::fs::set_permissions(&path, perms).unwrap();

        let got = store::read_audit_rounds(&dir).unwrap();

        // Restore before asserting so a failure cannot leave an unreadable
        // file behind in temp.
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o600);
        std::fs::set_permissions(&path, perms).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        match prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }

        // `assert_eq!` on the round count rather than `panic!` in a match arm:
        // `clippy::panic` is deny for this target and widening the crate-root
        // allow to satisfy one test would loosen the lint for every test in the
        // binary.
        let known_rounds = match &got {
            Determination::Known(rounds) => Some(rounds.len()),
            Determination::Undetermined(_) => None,
        };
        assert_eq!(
            known_rounds, None,
            "an unreadable ledger read back as a KNOWN history; 'I could not \
             open it' is not 'there is nothing in it'"
        );
    }

    #[test]
    fn concurrent_record_append_survives_audit_round_close_rewrite() {
        let _guard = store::HOME_ENV_LOCK.lock().unwrap();
        let prev_home = std::env::var_os("HOME");
        let dir = std::env::temp_dir().join(format!(
            "overwatch-audit-close-race-{}-{}",
            std::process::id(),
            store::now()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("HOME", &dir);
        // Widen close's locked read->rewrite window so the appender reliably
        // contends inside it.
        std::env::set_var("OVERWATCH_TEST_CLOSE_DELAY_MS", "300");

        // Seed round r1 (confirmed=9 so a later close to tests=7 is not clamped).
        let now = store::now();
        let r1 = AuditRound::new("r1".to_string(), &["overwatch".to_string()], 5, 9, 0, now);
        store::append_audit_round(&dir, &r1).unwrap();

        // Thread A: close r1 -> tests=7 (locked read-modify-write, delayed).
        let dir_a = dir.clone();
        let a = std::thread::spawn(move || {
            close_at(&dir_a, "r1", 7).expect("close_at ok");
        });

        // Give A a moment to acquire the lock and enter its delay, then run a
        // lock-respecting concurrent appender that records a NEW round r2.
        std::thread::sleep(std::time::Duration::from_millis(50));
        let r2 = AuditRound::new("r2".to_string(), &["overwatch".to_string()], 3, 2, 0, now);
        {
            let _l = LeaseLock::acquire(&dir);
            store::append_audit_round(&dir, &r2).unwrap();
        }

        a.join().unwrap();

        let rounds = store::read_audit_rounds(&dir)
            .unwrap()
            .require()
            .expect("the ledger written by this test must read back determined");
        let r1_out = rounds
            .iter()
            .find(|r| r.round == "r1")
            .expect("r1 must survive");
        assert_eq!(
            r1_out.regression_tests_added, 7,
            "close must persist r1's regression_tests_added"
        );
        assert!(
            rounds.iter().any(|r| r.round == "r2"),
            "concurrently appended r2 must not be lost by close's rewrite; got {:?}",
            rounds.iter().map(|r| r.round.as_str()).collect::<Vec<_>>()
        );

        std::env::remove_var("OVERWATCH_TEST_CLOSE_DELAY_MS");
        match prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }
}
