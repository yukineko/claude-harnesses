//! schemaguard — schema-validation gate for LLM structured outputs.
//!
#![deny(clippy::panic)]
//!
//! Validates a JSON value against a named declared schema, emits a structured
//! error (the re-ask contract) when invalid, and records reject counts so
//! silent drops at source→executor boundaries become observable.
//!
//! Exit codes:
//!   0  — JSON parsed and every check that ran passed (or `metrics`/`list`
//!        succeeded). The verdict also lists, under `not_checked`, the declared
//!        checks that were deliberately not performed, so a `valid: true` never
//!        implies more coverage than actually happened
//!   1  — JSON parsed but schema violations found
//!   2  — could not determine: JSON failed to parse, an unknown schema was
//!        requested, a declared check could not be applied to the value it was
//!        handed (reported under `undetermined`), or (`metrics`) the reject
//!        store exists but is unreadable
//!
//! This is a plain CLI, not a lifecycle hook — do not wrap in `run_hook`.

use std::io::Read;
use std::path::PathBuf;
use std::process::exit;

use clap::{Parser, Subcommand};
use harness_core::verdict::Verdict;
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

    // Validate. The report keeps three answers apart — checks that failed,
    // checks that could not be run, and checks deliberately not run — so this
    // verdict never has to guess which one an empty finding list meant.
    let report = schema::validate_report(&value, &schema.fields, "");
    let (code, out) = check_verdict(&schema.name, &report);

    // Both blocking answers are rejects: a payload that violated the schema and
    // a declared check that could not be applied are equally "did not pass", and
    // the counter exists so neither disappears silently. A clean verdict records
    // nothing.
    if code != 0 {
        metrics::record_reject(
            &schema.name,
            report.violations().len() + report.undetermined().len(),
        );
    }
    println!("{}", serde_json::to_string(&out).unwrap());
    code
}

/// Map a validation [`schema::Report`] onto this CLI's verdict: the exit code
/// and the JSON object to print.
///
/// Pure (no IO, no metrics side effect) so the three-answer resolution is
/// directly testable — including the `Undetermined` → exit 2 arm, which the five
/// registered schemas cannot currently reach end-to-end (every schema that
/// declares `items` also declares `Ty::Array`, so a non-array value is rejected
/// by the type check before the items constraint is reached).
///
/// `Verdict::exit_code` carries a single block code, and this CLI has two
/// distinct blocking codes (1 = checked and invalid, 2 = could not determine),
/// so the arms are spelled out here. Neither non-clean arm exits 0.
fn check_verdict(schema_name: &str, report: &schema::Report) -> (i32, serde_json::Value) {
    let errors: Vec<_> = report
        .violations()
        .iter()
        .map(|v| json!({"path": v.path, "problem": v.problem}))
        .collect();
    let undetermined: Vec<_> = report
        .undetermined()
        .iter()
        .map(|u| json!({"path": u.path, "problem": u.problem}))
        .collect();
    // Declared checks that were deliberately not performed. Emitted on every
    // schema-resolved verdict — including `valid: true` — because a consumer
    // reading `errors: []` would otherwise take it to mean every declared
    // constraint was evaluated and passed. Sometimes none of them were.
    let not_checked: Vec<_> = report
        .waived()
        .iter()
        .map(|w| json!({"path": w.path, "reason": w.reason}))
        .collect();

    match report.verdict() {
        Verdict::Clean(_) => (
            0,
            json!({
                "valid": true,
                "schema": schema_name,
                "errors": [],
                "not_checked": not_checked
            }),
        ),
        Verdict::Violation(_) => (
            1,
            json!({
                "valid": false,
                "schema": schema_name,
                "errors": errors,
                "undetermined": undetermined,
                "not_checked": not_checked
            }),
        ),
        // A declared constraint the engine could not apply. Not "valid", and not
        // a violation of the payload either — the honest answer is "cannot
        // determine", which is exit 2, the code this CLI already uses for an
        // unreadable input or an unknown schema.
        Verdict::Undetermined(_) => (
            2,
            json!({
                "valid": false,
                "schema": schema_name,
                "errors": errors,
                "undetermined": undetermined,
                "not_checked": not_checked
            }),
        ),
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

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use schemaguard::schema::{Field, Ty};

    static ITEM_FIELDS: &[Field] = &[Field {
        name: "id",
        ty: Ty::String,
        required: true,
        enum_values: &[],
        items: &[],
    }];

    /// A field whose declared `items` schema cannot be applied to a non-array
    /// value: the engine cannot tell whether the sub-schema holds.
    static INAPPLICABLE_ITEMS_FIELDS: &[Field] = &[Field {
        name: "config",
        ty: Ty::Any,
        required: true,
        enum_values: &[],
        items: ITEM_FIELDS,
    }];

    static SIMPLE_FIELDS: &[Field] = &[
        Field {
            name: "name",
            ty: Ty::String,
            required: true,
            enum_values: &[],
            items: &[],
        },
        Field {
            name: "role",
            ty: Ty::String,
            required: false,
            enum_values: &["admin", "user"],
            items: &[],
        },
    ];

    #[test]
    fn undetermined_check_exits_two_not_zero() {
        // The core invariant: a declared check the engine could not apply must
        // not leave through the `valid: true` / exit 0 door. Not reachable
        // through the five registered schemas today (see `check_verdict`'s
        // docs), which is why it is exercised here on the mapping itself.
        let value = json!({"config": {"id": "a"}});
        let report = schema::validate_report(&value, INAPPLICABLE_ITEMS_FIELDS, "");
        let (code, out) = check_verdict("test-schema", &report);
        assert_eq!(code, 2, "cannot-determine must exit 2, got {code}: {out}");
        assert_eq!(out["valid"], false, "got: {out}");
        assert!(
            !out["undetermined"].as_array().expect("array").is_empty(),
            "the verdict must say what could not be determined, got: {out}"
        );
    }

    #[test]
    fn violation_exits_one() {
        let value = json!({"role": "admin"}); // `name` is required and absent
        let report = schema::validate_report(&value, SIMPLE_FIELDS, "");
        let (code, out) = check_verdict("test-schema", &report);
        assert_eq!(code, 1, "a checked violation exits 1, got: {out}");
        assert_eq!(out["valid"], false);
    }

    #[test]
    fn clean_exits_zero_and_lists_what_it_did_not_check() {
        // `role` is absent and optional, so its enum constraint was never
        // evaluated: exit 0 is correct, silence about the un-run check is not.
        let value = json!({"name": "Alice"});
        let report = schema::validate_report(&value, SIMPLE_FIELDS, "");
        let (code, out) = check_verdict("test-schema", &report);
        assert_eq!(code, 0, "a declared permissive is not a block, got: {out}");
        assert_eq!(out["valid"], true);
        let not_checked = out["not_checked"].as_array().expect("array");
        assert!(
            not_checked.iter().any(|w| w["path"] == "role"),
            "the un-evaluated `role` enum must be named, got: {out}"
        );
    }

    #[test]
    fn clean_with_everything_checked_reports_nothing_not_checked() {
        // Anti-vacuity control for the test above: when every declared check did
        // run, `not_checked` must be empty rather than habitually populated.
        let value = json!({"name": "Alice", "role": "admin"});
        let report = schema::validate_report(&value, SIMPLE_FIELDS, "");
        let (code, out) = check_verdict("test-schema", &report);
        assert_eq!(code, 0);
        assert!(
            out["not_checked"].as_array().expect("array").is_empty(),
            "nothing was skipped here, got: {out}"
        );
    }
}
