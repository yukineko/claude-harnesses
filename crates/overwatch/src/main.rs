mod aggregate;
mod control;
mod event;
mod lease;
mod render;
mod store;

use anyhow::Result;
use clap::{Parser, Subcommand};

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
    }
    Ok(())
}
