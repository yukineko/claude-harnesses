//! hostaudit — configuration / operations security audit (scaffold).
//!
//! Contract: docs/hostaudit-baseline-v1.md. Every subcommand is declared but
//! NOT implemented yet: each exits 2 with "not implemented" on stderr, so no
//! caller can read this scaffold's output as a clean audit.

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "hostaudit",
    version,
    about = "Configuration / operations security audit"
)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// List audit targets (ssh config, --from-cmd, EC2).
    Inventory {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        rest: Vec<String>,
    },
    /// Choose which hosts may be connected to (`--all` allowed).
    Select {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        rest: Vec<String>,
    },
    /// Read-only ssh audit of selected hosts against baseline v1.
    Audit {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        rest: Vec<String>,
    },
    /// Audit local Claude Code settings.
    Local {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        rest: Vec<String>,
    },
    /// Read-only AWS audit; `aws apply` changes security groups on confirmation.
    Aws {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        rest: Vec<String>,
    },
    /// Print the ~/.daily/config.toml stanza / run as a daily task.
    DailyTask {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        rest: Vec<String>,
    },
}

fn main() {
    let cli = Cli::parse();
    let name = match cli.command {
        Cmd::Inventory { .. } => "inventory",
        Cmd::Select { .. } => "select",
        Cmd::Audit { .. } => "audit",
        Cmd::Local { .. } => "local",
        Cmd::Aws { .. } => "aws",
        Cmd::DailyTask { .. } => "daily-task",
    };
    eprintln!("hostaudit {name}: not implemented (scaffold); result is UNKNOWN");
    std::process::exit(2);
}
