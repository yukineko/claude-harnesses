#![deny(clippy::panic)]

mod aggregate;
pub mod audit_round;
mod audit_round_cli;
mod bridge;
pub mod canary;
mod canary_cli;
pub mod changeset;
mod control;
pub mod disposition;
mod disposition_cli;
pub mod event;
mod lease;
mod lock;
pub mod merge_conflict;
mod reconcile;
mod render;
mod review_escalation;
pub mod review_finding;
mod review_gate_decisions;
mod review_queue;
pub mod rollback;
mod rollback_cli;
pub mod store;
mod test_freshness;
pub mod violation;
mod violation_cli;

use anyhow::Result;
use clap::{Parser, Subcommand};
use violation::{RecurrencePolicy, ViolationSource};

#[derive(Parser)]
#[command(
    name = "overwatch",
    about = "Lease & event store for condukt orchestration"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Begin a new lease
    Begin {
        #[arg(long)]
        key: String,
        #[arg(long)]
        title: String,
        #[arg(long)]
        session: Option<String>,
        /// Comma-separated files/globs this session is responsible for (PDO
        /// session anchor, §4.2). Omit when the scope is not yet fixed.
        #[arg(long)]
        scope: Option<String>,
        /// This session's definition of "done" (re-anchored into the session's
        /// memory, §4.3).
        #[arg(long)]
        done_criteria: Option<String>,
    },
    /// Run a task within a lease
    Run {
        #[arg(long)]
        key: String,
        #[arg(long)]
        note: Option<String>,
    },
    /// End a lease with status
    End {
        #[arg(long)]
        key: String,
        #[arg(long)]
        status: String,
    },
    /// Send heartbeat for a lease
    Heartbeat {
        #[arg(long)]
        key: String,
    },
    /// Reap expired leases
    Reap,
    /// Show the live lease (PDO anchor) held by a session
    Lease {
        #[arg(long)]
        session: String,
        #[arg(long)]
        json: bool,
    },
    /// Show current status
    Status {
        #[arg(long)]
        json: bool,
    },
    /// List sessions
    Sessions {
        #[arg(long)]
        json: bool,
    },
    /// Pause a run
    Pause {
        #[arg(long)]
        run: String,
    },
    /// Resume a run
    Resume {
        #[arg(long)]
        run: String,
    },
    /// Reassign a lease
    Reassign {
        #[arg(long)]
        key: String,
        #[arg(long)]
        to: String,
    },
    /// Record a gate-violation event (blastguard/propguard/specguard/mutategate)
    /// with a normalized signature, for fleet-level correlated-error detection.
    RecordViolation {
        /// Source gate: blastguard | propguard | specguard | mutategate
        #[arg(long)]
        source: String,
        /// Source-specific discriminator (rule id / PROP id / drift kind / mutation operator).
        #[arg(long)]
        discriminator: String,
        /// Optional symbol (used by specguard alongside `discriminator` as drift kind).
        #[arg(long)]
        symbol: Option<String>,
        /// The task/content key this violation occurred against.
        #[arg(long)]
        task: String,
        #[arg(long)]
        session: Option<String>,
        /// Optional free-text detail for the audit trail (not used for signature matching).
        #[arg(long)]
        detail: Option<String>,
    },
    /// Show recurrence stats for all recorded violation signatures.
    Violations {
        #[arg(long)]
        json: bool,
        /// Minimum occurrences within the window to flag as systemic (default 3).
        #[arg(long)]
        threshold: Option<usize>,
        /// Recurrence window in seconds (default 86400 = 24h).
        #[arg(long)]
        window_secs: Option<i64>,
    },
    /// Show only signatures escalated as systemic issues.
    Escalations {
        #[arg(long)]
        json: bool,
        #[arg(long)]
        threshold: Option<usize>,
        #[arg(long)]
        window_secs: Option<i64>,
    },
    /// Deterministically split a plugin set into ordered canary stages and
    /// print the plan as JSON. Pure planning only — executes no rollout.
    CanaryPlan {
        /// Ordered plugin set (comma/space separated), in rollout order.
        #[arg(long)]
        plugins: String,
        /// Max plugins per stage (default 1 = most conservative canary).
        /// Takes precedence over --stage-count if both are given.
        #[arg(long)]
        stage_size: Option<usize>,
        /// Exactly this many stages (remainder distributed up front).
        #[arg(long)]
        stage_count: Option<usize>,
    },
    /// Evaluate the canary health gate against the item-B violation registry
    /// (or a supplied observed count) and print a PROCEED/ROLLBACK verdict as
    /// JSON. Deterministic: `now` is explicit, no wall-clock on the decision
    /// path. Exits non-zero when a rollback is advised, so callers can branch.
    CanaryGate {
        /// Raw observed violation count (pure mode; no clock, no store read).
        /// When omitted, the project's violation registry is read instead.
        #[arg(long)]
        observed_violations: Option<usize>,
        /// Max tolerated violations within the window before rollback.
        #[arg(long, default_value_t = 2)]
        threshold: usize,
        /// Problem-2.1b: dedicated threshold for the SYSTEMIC (fleet-recurrence)
        /// arm so it can trip INDEPENDENTLY of the raw-spike count (a systemic
        /// signature needs >= recurrence.threshold occurrences, so it would
        /// otherwise never fire below the shared raw threshold). Default 0 =
        /// any fleet-recurring signature since deploy advises rollback.
        #[arg(long, default_value_t = 0)]
        systemic_threshold: usize,
        /// Sliding window in seconds (registry mode only).
        #[arg(long, default_value_t = 900)]
        window_secs: i64,
        /// Count ONLY *systemic* recurring signatures (registry mode), the
        /// backward-compatible single-signal path. When omitted, registry mode
        /// emits BOTH a raw-spike and a systemic verdict and rolls back if
        /// EITHER fires (Problem-2.1).
        #[arg(long)]
        systemic: bool,
        /// Inject `now` (unix secs) for reproducible registry-mode evaluation.
        #[arg(long)]
        now: Option<i64>,
        /// Stage-deploy anchor (unix secs): in registry mode, exclude any
        /// violation with `ts < since` so pre-deploy noise is not attributed
        /// to the canary stage (Problem-2.2). Omit for no lower bound.
        #[arg(long)]
        since: Option<i64>,
    },
    /// Compute a canary rollback plan (what to restore) as JSON, from prior
    /// install state + canary targets passed as inline JSON. Pure data only —
    /// re-points nothing; the shell acts on it under its opt-in flag.
    CanaryRollbackPlan {
        /// Stage index this rollback plan applies to.
        #[arg(long, default_value_t = 0)]
        stage_index: usize,
        /// JSON array of prior install state (name/prior_version/
        /// prior_install_path) captured before the stage.
        #[arg(long)]
        prior: String,
        /// JSON array of canary targets (name/canary_version/
        /// canary_install_path) the stage moved plugins to.
        #[arg(long)]
        canary_targets: String,
    },
    /// Record a canary rollback event to the overwatch-readable rollback log,
    /// so `review-queue` can surface it. Fail-soft: a store-write error is
    /// reported but never breaks the caller (the rollout script). Called by
    /// `scripts/rollout-plugins.sh` when the health gate executes a rollback.
    RecordRollback {
        /// The plugin that was rolled back.
        #[arg(long)]
        plugin: String,
        /// The version restored TO (prior version). Omit for a plugin the
        /// canary newly introduced (nothing to restore).
        #[arg(long)]
        from_version: Option<String>,
        /// The canary version rolled back FROM.
        #[arg(long)]
        to_version: String,
        /// The 0-based canary stage index the rollback halted at.
        #[arg(long, default_value_t = 0)]
        stage: usize,
        /// Why the gate advised the rollback: raw | systemic.
        #[arg(long, default_value = "raw")]
        reason: String,
        /// Optional free-text detail for the audit trail.
        #[arg(long)]
        detail: Option<String>,
    },
    /// Record an AI/adversarial review finding to the overwatch-readable
    /// findings store (the defined ingestion point for the future
    /// Continuous-Audit loop). `review-queue` reads these back.
    RecordFinding {
        /// Stable identifier for this finding.
        #[arg(long)]
        finding_id: String,
        /// Which reviewer/tool produced it (e.g. reviewgate, auditmap).
        #[arg(long)]
        source: String,
        /// Severity as reported (high/med/low), optional.
        #[arg(long)]
        severity: Option<String>,
        /// Short human summary of the finding.
        #[arg(long)]
        summary: String,
        /// Primary file the finding concerns, optional.
        #[arg(long)]
        file: Option<String>,
        /// The verifier's rationale for confirming this finding (e.g. a
        /// file:line quoted argument for why it's real), optional.
        #[arg(long)]
        rationale: Option<String>,
        /// The adversarial verifier's TRI-state verdict:
        /// `confirmed` | `refuted` | `unverified`.
        ///
        /// Any unrecognized value is recorded as `unverified` (undetermined
        /// resolves to the restrictive side — never silently confirmed, never
        /// dropped). Omitting the flag records `confirmed`, preserving the
        /// pre-tri-state contract where this command ingested ONLY the
        /// verifier's CONFIRMED subset; say `--verdict unverified` explicitly
        /// when the verifier could not settle the claim. Only `confirmed`
        /// findings are forwarded by `review-queue --to-backlog`;
        /// `unverified` ones stay visible in the queue, marked, pending
        /// re-verification.
        #[arg(long)]
        verdict: Option<String>,
    },
    /// Continuous-Audit round metrics ledger (2630b4c5). `record` appends one
    /// round's counts to the convergence ledger that `audit-metrics` reads back.
    AuditRound {
        #[command(subcommand)]
        action: AuditRoundAction,
    },
    /// Read the Continuous-Audit round ledger and print convergence metrics:
    /// per-round new-findings trend, closure-rate (regression tests ÷
    /// confirmed), and a `converging` flag. Fail-soft: an empty ledger prints a
    /// zero-round report.
    AuditMetrics {
        #[arg(long)]
        json: bool,
        /// How many trailing rounds the `converging` check considers (default 3;
        /// 0 = all rounds).
        #[arg(long)]
        window: Option<usize>,
    },
    /// Record a human disposition (confirmed|dismissed|false-positive) of an
    /// AI/adversarial review finding (join key: `--finding-id`, resolved
    /// against `record-finding`). `review-metrics` reads these back to
    /// compute false-positive rate / agreement rate / median latency.
    RecordDisposition {
        /// The finding_id this disposition resolves (joins to `record-finding`).
        #[arg(long = "finding-id")]
        finding_id: String,
        /// The human verdict: confirmed | dismissed | false-positive.
        #[arg(long)]
        verdict: String,
        /// Free-text identifier of who resolved it.
        #[arg(long)]
        reviewer: String,
    },
    /// Read the disposition ledger (joined against the review-findings
    /// store) and print review-effectiveness metrics: false-positive rate,
    /// human-agreement rate, and median resolution latency. Fail-soft: an
    /// empty ledger prints a zero/`n/a` report rather than erroring.
    ReviewMetrics {
        #[arg(long)]
        json: bool,
    },
    /// Resolve a blocked merge (design 625aa170 B): a real git 3-way conflict
    /// or a gated mid-flight overlap surfaced in `review-queue` as
    /// `[merge-conflict]`. Records a resolution (join key: `--id`) so the entry
    /// leaves the open set and condukt's reconciliation driver can act on it.
    /// Escalate/Block only on the policy side — `--choose` is the human's
    /// explicit pick (never an auto last-writer-wins). Idempotent per id.
    ResolveMergeConflict {
        /// The conflict_id to resolve (from `review-queue --json`).
        #[arg(long = "id")]
        id: String,
        /// Which side to keep: ours | theirs | manual.
        #[arg(long)]
        choose: String,
        /// Who decided: human | policy (default human).
        #[arg(long, default_value = "human")]
        by: String,
        /// Optional free-text note recorded with the resolution.
        #[arg(long)]
        note: Option<String>,
    },
    /// The unified human review surface: merge systemic gate violations, canary
    /// rollback events, and AI-review findings into ONE risk-ordered list
    /// (highest normalized severity first, newest-first within a severity
    /// band), each row tagged with its source kind. Fail-soft: a
    /// missing/empty source contributes nothing rather than erroring; the
    /// other sources still render.
    ReviewQueue {
        #[arg(long)]
        json: bool,
        /// Only show entries with `ts >= since` (unix seconds).
        #[arg(long)]
        since: Option<i64>,
        /// Cap the number of rows shown to the top-K riskiest (after
        /// severity-first ordering); shed lower-risk rows are reported.
        #[arg(long)]
        limit: Option<usize>,
        /// Drain the WHOLE review queue to the backlog instead of rendering:
        /// each not-yet-bridged entry (AI finding, systemic violation, canary
        /// rollback, or condukt escalation) is forwarded via `backlog add`.
        /// Idempotent — findings on `bridged_findings.jsonl` (bare finding-id),
        /// the other three streams on `bridged_entries.jsonl`
        /// (`<kind>:<identifier>`). Fail-soft: a missing store / absent backlog
        /// / failed add is warned and skipped; the command still succeeds.
        #[arg(long = "to-backlog")]
        to_backlog: bool,
    },
    /// Compact the review-findings store: move every finding whose
    /// finding_id has been resolved (bridged to the backlog, or
    /// dispositioned by a human) out of the hot review_findings.jsonl into a
    /// cold review_findings_archive.jsonl. Non-lossy (archive, never
    /// delete): `review-metrics` keeps joining archived findings via the
    /// combined hot-plus-archive history, while `review-queue` keeps reading
    /// the hot file only (now bounded to OPEN items). Atomic (temp+rename)
    /// and idempotent: a run with no newly-resolved findings is a no-op.
    CompactFindings {
        #[arg(long)]
        json: bool,
    },
    /// Companion to `review-queue`: surface the DENOMINATOR — the population
    /// of decisions condukt auto-approved (self-answered without a human,
    /// per `condukt policy answer`'s `gate-decisions.jsonl` journal) — as a
    /// count plus a deterministic seeded sample, so a human can judge
    /// whether spot-check sampling coverage is adequate. Fail-soft: a
    /// missing/unreadable/corrupt journal contributes zero rows rather than
    /// erroring.
    AutoApproved {
        #[arg(long)]
        json: bool,
        /// Only count/sample decisions with `created_at >= since` (absolute
        /// unix seconds; no wall-clock in the pure core).
        #[arg(long)]
        since: Option<i64>,
        /// How many records to sample from the (windowed) population.
        #[arg(long, default_value_t = 5)]
        sample: usize,
        /// Seed for the deterministic sample draw.
        #[arg(long, default_value_t = 0)]
        seed: u64,
    },
    /// Scan a range of git commit messages for `CA-<crate>-<NNN>` finding-id
    /// references and auto-record a CONFIRMED disposition for any referenced
    /// finding that is on the review-findings store but not yet dispositioned
    /// (closes the "fix landed, nobody ran record-disposition" gap that lets
    /// review-queue go stale). Idempotent: already-dispositioned finding-ids
    /// are skipped. Fail-soft: a git failure or unreadable store degrades to
    /// "0 processed" rather than erroring, so this is always safe to wire
    /// into a pre-push hook or a Continuous-Audit round.
    ReconcileFixed {
        /// Scan every commit since this ref (exclusive), i.e. `<ref>..HEAD`.
        #[arg(long = "since-ref", conflicts_with_all = ["range", "last_n"])]
        since_ref: Option<String>,
        /// A raw git revision range/expression, passed to `git log` as-is.
        #[arg(long, conflicts_with_all = ["since_ref", "last_n"])]
        range: Option<String>,
        /// Scan only the most recent N commits (default: 50 when no other
        /// range selector is given).
        #[arg(long = "last-n", conflicts_with_all = ["since_ref", "range"])]
        last_n: Option<usize>,
        /// Report what WOULD be reconciled without writing any dispositions.
        #[arg(long = "dry-run")]
        dry_run: bool,
        #[arg(long)]
        json: bool,
    },
}

/// Actions under `overwatch audit-round`.
#[derive(Subcommand)]
enum AuditRoundAction {
    /// Append one Continuous-Audit round's metrics to the convergence ledger.
    Record {
        /// The round identifier (caller-assigned opaque label, e.g. an ISO
        /// week `2026W28`, a date, or a sequence number).
        #[arg(long)]
        round: String,
        /// The crate(s) this round reviewed (comma/space separated).
        #[arg(long)]
        target: String,
        /// How many NEW findings the finder surfaced this round.
        #[arg(long, default_value_t = 0)]
        new_findings: u64,
        /// How many findings the verifier CONFIRMED this round.
        #[arg(long, default_value_t = 0)]
        confirmed: u64,
        /// How many findings ended UNVERIFIED this round (the verifier could
        /// neither establish nor discharge them). Recorded separately so
        /// `new_findings - confirmed` is not silently read as "all refuted" —
        /// an undetermined claim is still open.
        #[arg(long, default_value_t = 0)]
        unverified: u64,
        /// How many confirmed findings were converted into regression tests.
        #[arg(long, default_value_t = 0)]
        regression_tests_added: u64,
        /// The model the FINDER stage used this round (optional). When BOTH
        /// finder/verifier models are supplied and are the SAME model, the
        /// `finder != verifier` MUST is violated and a high-severity warning
        /// finding is recorded into the review queue (fail-soft; the round is
        /// still recorded and the loop is never broken). Omit both for the
        /// original, unchecked behavior.
        #[arg(long)]
        finder_model: Option<String>,
        /// The model the VERIFIER stage used this round (optional; see
        /// `--finder-model`).
        #[arg(long)]
        verifier_model: Option<String>,
    },
    /// Close a round: SET its regression_tests_added to `--tests` (the fix-side
    /// feedback recorded AFTER confirmed findings are converted to regression
    /// tests, which `record` cannot know at finding-time). Idempotent (SET, not
    /// add); an unknown round-id leaves the ledger unchanged (fail-soft). When a
    /// round-id is duplicated, the most-recently recorded match is closed.
    Close {
        /// The round identifier to close (the same id passed to `record`).
        #[arg(long)]
        round: String,
        /// Regression tests locked in for this round's confirmed findings.
        /// REQUIRED (no default): SET-not-add semantics mean a bare `close`
        /// would otherwise silently reset a round's progress to 0.
        #[arg(long)]
        tests: u64,
    },
}

/// Parse a `--source` CLI value into a [`ViolationSource`], erroring clearly
/// on an unrecognized token rather than silently defaulting.
fn parse_source(s: &str) -> Result<ViolationSource> {
    ViolationSource::parse(&s.to_lowercase()).ok_or_else(|| {
        anyhow::anyhow!(
            "unknown violation source: {s} (expected blastguard|propguard|specguard|mutategate)"
        )
    })
}

/// Build a [`RecurrencePolicy`] from optional CLI overrides, falling back to defaults.
fn resolve_policy(threshold: Option<usize>, window_secs: Option<i64>) -> RecurrencePolicy {
    let default = RecurrencePolicy::default();
    RecurrencePolicy {
        threshold: threshold.unwrap_or(default.threshold),
        window_secs: window_secs.unwrap_or(default.window_secs),
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Begin {
            key,
            title,
            session,
            scope,
            done_criteria,
        } => {
            // Split the comma-separated scope into non-empty trimmed globs.
            let scope: Vec<String> = scope
                .as_deref()
                .map(|s| {
                    s.split(',')
                        .map(str::trim)
                        .filter(|p| !p.is_empty())
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            lease::begin(&key, &title, session.as_deref(), scope, done_criteria)?;
        }
        Command::Run { key, note } => {
            lease::run(&key, note.as_deref())?;
        }
        Command::End { key, status } => {
            lease::end(&key, &status)?;
        }
        Command::Heartbeat { key } => {
            lease::heartbeat(&key)?;
        }
        Command::Reap => {
            lease::reap()?;
        }
        Command::Lease { session, json } => {
            lease::lease_for_session(&session, json)?;
        }
        Command::Status { json } => {
            render::status(json)?;
        }
        Command::Sessions { json } => {
            render::sessions(json)?;
        }
        Command::Pause { run } => {
            control::pause(&run)?;
        }
        Command::Resume { run } => {
            control::resume(&run)?;
        }
        Command::Reassign { key, to } => {
            control::reassign(&key, &to)?;
        }
        Command::RecordViolation {
            source,
            discriminator,
            symbol,
            task,
            session,
            detail,
        } => {
            let src = parse_source(&source)?;
            violation_cli::record(
                src,
                &discriminator,
                symbol.as_deref(),
                &task,
                session.as_deref(),
                detail.as_deref(),
            )?;
        }
        Command::Violations {
            json,
            threshold,
            window_secs,
        } => {
            let policy = resolve_policy(threshold, window_secs);
            violation_cli::print_recurrence(policy, json)?;
        }
        Command::Escalations {
            json,
            threshold,
            window_secs,
        } => {
            let policy = resolve_policy(threshold, window_secs);
            violation_cli::print_escalations(policy, json)?;
        }
        Command::CanaryPlan {
            plugins,
            stage_size,
            stage_count,
        } => {
            canary_cli::plan(&plugins, stage_size, stage_count)?;
        }
        Command::CanaryGate {
            observed_violations,
            threshold,
            systemic_threshold,
            window_secs,
            systemic,
            now,
            since,
        } => {
            let rollback = canary_cli::gate(
                observed_violations,
                threshold,
                systemic_threshold,
                window_secs,
                systemic,
                now,
                since,
            )?;
            if rollback {
                // Non-zero exit signals "rollback advised" to the shell so it
                // can branch without parsing JSON, while the JSON verdict is
                // still emitted for logging/inspection.
                std::process::exit(3);
            }
        }
        Command::CanaryRollbackPlan {
            stage_index,
            prior,
            canary_targets,
        } => {
            canary_cli::rollback_plan(stage_index, &prior, &canary_targets)?;
        }
        Command::RecordRollback {
            plugin,
            from_version,
            to_version,
            stage,
            reason,
            detail,
        } => {
            rollback_cli::record(
                &plugin,
                from_version.as_deref(),
                &to_version,
                stage,
                &reason,
                detail.as_deref(),
            )?;
        }
        Command::RecordFinding {
            finding_id,
            source,
            severity,
            summary,
            file,
            rationale,
            verdict,
        } => {
            rollback_cli::record_finding(
                &finding_id,
                &source,
                severity.as_deref(),
                &summary,
                file.as_deref(),
                rationale.as_deref(),
                verdict.as_deref(),
            )?;
        }
        Command::AuditRound { action } => match action {
            AuditRoundAction::Record {
                round,
                target,
                new_findings,
                confirmed,
                unverified,
                regression_tests_added,
                finder_model,
                verifier_model,
            } => {
                audit_round_cli::record(
                    round,
                    &target,
                    new_findings,
                    confirmed,
                    unverified,
                    regression_tests_added,
                    finder_model.as_deref(),
                    verifier_model.as_deref(),
                )?;
            }
            AuditRoundAction::Close { round, tests } => {
                audit_round_cli::close(round, tests)?;
            }
        },
        Command::AuditMetrics { json, window } => {
            audit_round_cli::metrics(json, window)?;
        }
        Command::RecordDisposition {
            finding_id,
            verdict,
            reviewer,
        } => {
            disposition_cli::record(finding_id, &verdict, reviewer, store::now())?;
        }
        Command::ReviewMetrics { json } => {
            disposition_cli::metrics(json)?;
        }
        Command::ResolveMergeConflict {
            id,
            choose,
            by,
            note,
        } => {
            merge_conflict::record_resolution(id, &choose, &by, note, store::now())?;
        }
        Command::ReviewQueue {
            json,
            since,
            limit,
            to_backlog,
        } => {
            if to_backlog {
                bridge::to_backlog()?;
            } else {
                review_queue::run(json, since, limit)?;
            }
        }
        Command::AutoApproved {
            json,
            since,
            sample,
            seed,
        } => {
            run_auto_approved(json, since, sample, seed)?;
        }
        Command::CompactFindings { json } => {
            run_compact_findings(json)?;
        }
        Command::ReconcileFixed {
            since_ref,
            range,
            last_n,
            dry_run,
            json,
        } => {
            let range = if let Some(r) = since_ref {
                reconcile::ReconcileRange::SinceRef(r)
            } else if let Some(r) = range {
                reconcile::ReconcileRange::Range(r)
            } else {
                reconcile::ReconcileRange::LastN(last_n.unwrap_or(50))
            };
            reconcile::run(range, dry_run, json)?;
        }
    }
    Ok(())
}

/// Handler for `overwatch compact-findings`: move resolved (bridged or
/// dispositioned) review findings out of the hot review_findings.jsonl into
/// the cold review_findings_archive.jsonl, keeping the hot file bounded to
/// OPEN items while the review-metrics latency join reads hot plus archive
/// (`store::read_review_findings_all`) so nothing regresses. See
/// `store::compact_review_findings` for the pure/atomic core.
fn run_compact_findings(json: bool) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let report = store::compact_review_findings(&cwd)?;

    if json {
        let out = serde_json::json!({
            "open": report.open,
            "archived": report.archived,
            "already_archived": report.already_archived,
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    println!(
        "compacted: {} open, {} archived ({} already archived)",
        report.open, report.archived, report.already_archived
    );
    Ok(())
}

/// Handler for `overwatch auto-approved`: read condukt's auto-approved
/// gate-decision journal (fail-soft), window it with `since`, and print the
/// count plus a deterministic seeded sample — either as human-readable text
/// or as JSON. See [`review_gate_decisions`] for the pure core this wraps.
fn run_auto_approved(json: bool, since: Option<i64>, sample: usize, seed: u64) -> Result<()> {
    let population = review_gate_decisions::read_auto_approved();
    let filtered = review_gate_decisions::filter_since(&population, since);
    let count = filtered.len();
    let picked = review_gate_decisions::sample_auto_approved(&filtered, sample, seed);

    if json {
        let out = serde_json::json!({
            "count": count,
            "since": since,
            "seed": seed,
            "sample_size": picked.len(),
            "sample": picked,
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    match since {
        Some(ts) => println!(
            "auto-approved: {count} decision(s) passed a gate without human review since {ts}"
        ),
        None => println!("auto-approved: {count} decision(s) passed a gate without human review"),
    }
    if count == 0 {
        println!("(no auto-approved decisions found)");
        return Ok(());
    }
    println!("sample ({} of {count}, seed {seed}):", picked.len());
    for d in &picked {
        println!(
            "  [{}] chosen={:?} question={:?}",
            d.created_at, d.chosen, d.question
        );
    }
    Ok(())
}
