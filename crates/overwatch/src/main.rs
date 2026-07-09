mod aggregate;
pub mod canary;
mod canary_cli;
mod control;
pub mod event;
mod lease;
mod render;
pub mod store;
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
        /// Sliding window in seconds (registry mode only).
        #[arg(long, default_value_t = 900)]
        window_secs: i64,
        /// Count only *systemic* recurring signatures (registry mode) rather
        /// than raw violations, so isolated one-offs don't trip a rollback.
        #[arg(long)]
        systemic: bool,
        /// Inject `now` (unix secs) for reproducible registry-mode evaluation.
        #[arg(long)]
        now: Option<i64>,
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
        } => {
            lease::begin(&key, &title, session.as_deref())?;
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
            window_secs,
            systemic,
            now,
        } => {
            let rollback =
                canary_cli::gate(observed_violations, threshold, window_secs, systemic, now)?;
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
    }
    Ok(())
}
