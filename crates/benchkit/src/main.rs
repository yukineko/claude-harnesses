//! benchkit CLI — SWE-bench Verified runner skeleton.
//!
//! `benchkit load` reads a JSONL split into typed instances (deterministic,
//! offline). `benchkit download` fetches the upstream dataset via `curl` into a
//! local cache — the only subcommand that touches the network, and only when
//! explicitly invoked.

use std::path::PathBuf;
use std::process::exit;

use clap::{Parser, Subcommand};

use benchkit::download::{self, Outcome};
use benchkit::loader;

#[derive(Parser)]
#[command(
    name = "benchkit",
    version,
    about = "SWE-bench Verified benchmark runner (typed instance model + JSONL loader + gated download)."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Load a JSONL split into typed instances and print a deterministic summary.
    Load {
        /// Path to a JSONL file (one Instance per line).
        path: PathBuf,
    },
    /// Fetch SWE-bench Verified into a local cache (idempotent; network only here).
    Download {
        /// Cache file path (default: .benchkit-cache/swe-bench-verified.jsonl).
        #[arg(long)]
        dest: Option<PathBuf>,
        /// Re-fetch even if the cache already exists.
        #[arg(long)]
        force: bool,
    },
}

fn main() {
    let cli = Cli::parse();
    let code = match cli.command {
        Command::Load { path } => match loader::load_instances(&path) {
            Ok(instances) => {
                println!(
                    "loaded {} instances from {}",
                    instances.len(),
                    path.display()
                );
                for inst in &instances {
                    println!("  {} ({})", inst.instance_id, inst.repo);
                }
                0
            }
            Err(e) => {
                eprintln!("error: {e:#}");
                1
            }
        },
        Command::Download { dest, force } => match download::execute(dest, force) {
            Ok(Outcome::CacheHit(p)) => {
                println!("cache hit (no fetch): {}", p.display());
                0
            }
            Ok(Outcome::Fetched(p)) => {
                println!("fetched: {}", p.display());
                0
            }
            Err(e) => {
                eprintln!("error: {e:#}");
                1
            }
        },
    };
    exit(code);
}
