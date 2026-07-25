//! schemaguard — schema-validation gate for LLM structured outputs.
//!
#![deny(clippy::panic)]
//!
//! Validates a JSON value against a named declared schema, emits a structured
//! error (the re-ask contract) when invalid, and records reject counts so
//! silent drops at source→executor boundaries become observable.
//!
//! Exit codes:
//!   0  — JSON parsed and schema valid (or `metrics`/`list` succeeded)
//!   1  — JSON parsed but schema violations found
//!   2  — could not determine: JSON failed to parse, an unknown schema was
//!        requested, or (`metrics`) the reject store exists but is unreadable
//!
//! This is a plain CLI, not a lifecycle hook — do not wrap in `run_hook`.

use std::io::Read;
use std::path::PathBuf;
use std::process::exit;

use clap::{Parser, Subcommand};
use schemaguard::{metrics, registry, schema};
use serde_json::json;

// ── CLI definition ────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(
    name = "schemaguard",
    version,
    about = "Schema-validation gate for LLM structured outputs at source→executor boundaries."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Validate a JSON value against a named schema.
    Check(CheckArgs),
    /// Print reject counts per schema.
    Metrics(MetricsArgs),
    /// List known schema names.
    List,
}

#[derive(clap::Args)]
struct CheckArgs {
    /// Schema name to validate against (see `schemaguard list`).
    #[arg(long)]
    schema: String,
    /// Path to a JSON file; reads from stdin if omitted.
    #[arg(long)]
    file: Option<PathBuf>,
}

#[derive(clap::Args)]
struct MetricsArgs {
    /// Emit JSON instead of a human-readable table.
    #[arg(long)]
    json: bool,
}

// ── command handlers ──────────────────────────────────────────────────────────

fn cmd_check(args: CheckArgs) -> i32 {
    // Resolve schema first so we can fail fast before reading any input.
    let schema = match registry::get(&args.schema) {
        Some(s) => s,
        None => {
            let known = registry::names().join(", ");
            eprintln!(
                "schemaguard: unknown schema '{}' (known: {})",
                args.schema, known
            );
            return 2;
        }
    };

    // Read input
    let raw = match args.file {
        Some(ref path) => match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                let out = json!({
                    "valid": false,
                    "error": format!("cannot read file {}: {}", path.display(), e)
                });
                println!("{}", serde_json::to_string(&out).unwrap());
                return 2;
            }
        },
        None => {
            let mut buf = String::new();
            if let Err(e) = std::io::stdin().read_to_string(&mut buf) {
                let out = json!({"valid": false, "error": format!("cannot read stdin: {}", e)});
                println!("{}", serde_json::to_string(&out).unwrap());
                return 2;
            }
            buf
        }
    };

    // Parse JSON
    let value: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            // Parse failure counts as a reject
            metrics::record_reject(&schema.name, 1);
            let out = json!({
                "valid": false,
                "error": format!("invalid JSON: {}", e)
            });
            println!("{}", serde_json::to_string(&out).unwrap());
            return 2;
        }
    };

    // Validate
    let violations = schema::validate(&value, &schema.fields, "");

    if violations.is_empty() {
        let out = json!({
            "valid": true,
            "schema": schema.name,
            "errors": []
        });
        println!("{}", serde_json::to_string(&out).unwrap());
        0
    } else {
        let error_count = violations.len();
        metrics::record_reject(&schema.name, error_count);
        let errors: Vec<_> = violations
            .iter()
            .map(|v| json!({"path": v.path, "problem": v.problem}))
            .collect();
        let out = json!({
            "valid": false,
            "schema": schema.name,
            "errors": errors
        });
        println!("{}", serde_json::to_string(&out).unwrap());
        1
    }
}

fn cmd_metrics(args: MetricsArgs) -> i32 {
    // An unreadable store is "cannot determine", not "nothing to report". Printing
    // an empty result here would be indistinguishable from a genuinely empty store
    // and would read as "no rejects ever happened" — the inverse of what this
    // counter exists to show. `require()` is the only extractor, so the permissive
    // path is not expressible.
    let counts = match metrics::counts().require() {
        Ok(c) => c,
        Err(verdict) => {
            let why = verdict
                .reason()
                .map(|r| r.as_str().to_string())
                .unwrap_or_else(|| "reject store could not be read".to_string());
            if args.json {
                let out = json!({"status": "unknown", "error": why});
                println!("{}", serde_json::to_string(&out).unwrap_or_default());
            } else {
                println!("unknown — reject counts could not be determined");
            }
            eprintln!("schemaguard: cannot determine reject counts: {why}");
            return 2;
        }
    };
    if args.json {
        println!("{}", serde_json::to_string_pretty(&counts).unwrap());
    } else {
        if counts.is_empty() {
            println!("No rejects recorded yet.");
        } else {
            println!("{:<20} rejects", "schema");
            println!("{}", "-".repeat(32));
            for (schema, count) in &counts {
                println!("{:<20} {}", schema, count);
            }
        }
    }
    0
}

fn cmd_list() -> i32 {
    for name in registry::names() {
        println!("{}", name);
    }
    0
}

// ── entry point ───────────────────────────────────────────────────────────────

fn main() {
    let cli = Cli::parse();
    let code = match cli.command {
        Command::Check(args) => cmd_check(args),
        Command::Metrics(args) => cmd_metrics(args),
        Command::List => cmd_list(),
    };
    exit(code);
}
