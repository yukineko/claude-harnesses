//! webqa — Web QA gate plugin (Playwright).
mod weaken;

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "webqa", version)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Stop hook gate. Not implemented yet: fails closed (always blocks).
    Gate,
    /// Detect test-weakening edits to Playwright specs.
    /// Exit 0 clean, 1 findings, 2 undetermined.
    WeakenCheck {
        /// Read a unified diff from this file instead of `git diff --cached`.
        #[arg(long)]
        diff_file: Option<PathBuf>,
    },
}

fn main() {
    let cli = Cli::parse();
    let code = match cli.cmd {
        Cmd::Gate => {
            println!(
                "{}",
                serde_json::json!({
                    "decision": "block",
                    "reason": "webqa gate not implemented yet"
                })
            );
            0
        }
        Cmd::WeakenCheck { diff_file } => weaken::report(&weaken::run(diff_file.as_deref())),
    };
    std::process::exit(code);
}
