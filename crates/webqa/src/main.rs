//! webqa — Web QA gate plugin.
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::io::Read;

mod weaken;

#[derive(Parser)]
#[command(name = "webqa", version)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Stop hook gate (stub).
    Gate,
    /// Check for test-weakening patterns in Playwright spec diffs.
    #[command(name = "weaken-check")]
    WeakenCheck {
        /// Path to diff file (stdin if not provided).
        #[arg(long)]
        diff_file: Option<PathBuf>,
    },
}

fn main() {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Gate => {}
        Cmd::WeakenCheck { diff_file } => {
            let result = if let Some(path) = diff_file {
                weaken::check_diff_file(&path)
            } else {
                // Read from stdin
                let mut content = String::new();
                if std::io::stdin().read_to_string(&mut content).is_err() {
                    std::process::exit(2);
                }
                weaken::check_diff_content(&content)
            };

            for finding in result.findings {
                println!("{}", finding);
            }
            std::process::exit(result.exit_code as i32);
        }
    }
}
