mod aggregate;
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
    }
    Ok(())
}
