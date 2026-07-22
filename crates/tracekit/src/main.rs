//! tracekit — span-tree tracer for condukt runs.
//!
//! Records one run's phases (interpreter→worker→verifier) as a parent-linked
//! span tree, renders it (`tracekit trace <RID>`), and exports OTel GenAI-semconv
//! JSON (`tracekit export <RID>`). gauge buckets cost by agent *kind* with no
//! run/task/span linkage; tracekit adds the missing per-run causal tree so you
//! can see which phase of a failed run was slow, expensive, or broke.
//!
//! A plain CLI (callers — condukt's state-set transitions, or a human — invoke
//! `tracekit record`), not a lifecycle hook, so it does not wrap work in
//! `run_hook`. File-only, no network, no API key.

mod otlp;
mod span;
mod trace;

use std::path::{Path, PathBuf};
use std::process::exit;
use std::time::{SystemTime, UNIX_EPOCH};

use clap::{Args, Parser, Subcommand};

use span::Span;

#[derive(Parser)]
#[command(
    name = "tracekit",
    version,
    about = "Span-tree tracer for condukt runs (record / trace / export OTel GenAI spans)."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Append one span to a run's store (called when a phase finishes).
    Record(RecordArgs),
    /// Render a run's span tree.
    Trace { run_id: String },
    /// Export a run's spans as OTLP/JSON (OTel GenAI semconv).
    Export(ExportArgs),
    /// List runs that have recorded spans.
    List,
}

#[derive(Args)]
struct RecordArgs {
    /// Run id (condukt RID) this span belongs to.
    #[arg(long)]
    run: String,
    /// Stable span id (e.g. the condukt task id).
    #[arg(long)]
    span: String,
    /// Parent span id; omit for the run root.
    #[arg(long)]
    parent: Option<String>,
    /// Human-readable span name.
    #[arg(long)]
    name: String,
    /// Phase bucket: interpreter | worker | verifier | tool | …
    #[arg(long)]
    phase: String,
    #[arg(long)]
    model: Option<String>,
    #[arg(long)]
    task: Option<String>,
    /// Phase duration in milliseconds.
    #[arg(long, default_value_t = 0)]
    ms: u64,
    /// Phase cost in USD.
    #[arg(long)]
    cost: Option<f64>,
    /// Terminal status: ok | error | verified | failed | …
    #[arg(long, default_value = "ok")]
    status: String,
    /// Override the record-time end timestamp (epoch millis). Defaults to now;
    /// pass for deterministic replay.
    #[arg(long)]
    end_unix_ms: Option<u64>,
}

#[derive(Args)]
struct ExportArgs {
    run_id: String,
    /// `service.name` resource attribute.
    #[arg(long, default_value = "condukt")]
    service: String,
    /// Output path; `-` writes to stdout. Defaults to
    /// `~/.tracekit/<RID>/otlp-<RID>.json`.
    #[arg(long)]
    out: Option<String>,
}

fn main() {
    let cli = Cli::parse();
    let code = match cli.command {
        Command::Record(a) => cmd_record(a),
        Command::Trace { run_id } => cmd_trace(&run_id),
        Command::Export(a) => cmd_export(a),
        Command::List => cmd_list(),
    };
    exit(code);
}

fn cmd_record(a: RecordArgs) -> i32 {
    let s = Span {
        run_id: a.run,
        span_id: a.span,
        parent_id: a.parent,
        name: a.name,
        phase: a.phase,
        model: a.model,
        task_id: a.task,
        ms: a.ms,
        cost_usd: a.cost,
        status: a.status,
        end_unix_ms: a.end_unix_ms.unwrap_or_else(now_unix_ms),
    };
    match span::append(&s) {
        Ok(path) => {
            eprintln!("tracekit: recorded {} → {}", s.span_id, path.display());
            0
        }
        Err(e) => {
            eprintln!("tracekit: {e:#}");
            1
        }
    }
}

fn cmd_trace(run_id: &str) -> i32 {
    match span::load(run_id) {
        Ok((spans, skipped)) => {
            print!("{}", trace::render(run_id, &spans, skipped));
            0
        }
        Err(e) => {
            eprintln!("tracekit: {e:#}");
            2
        }
    }
}

fn cmd_export(a: ExportArgs) -> i32 {
    let (spans, skipped) = match span::load(&a.run_id) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("tracekit: {e:#}");
            return 2;
        }
    };
    if skipped > 0 {
        eprintln!("tracekit: skipped {skipped} malformed span line(s)");
    }
    let doc = otlp::to_otlp(&a.service, &spans);
    let text = serde_json::to_string_pretty(&doc).unwrap_or_else(|_| "{}".to_string());

    match a.out.as_deref() {
        Some("-") => {
            println!("{text}");
            0
        }
        other => {
            let path = match other {
                Some(p) => PathBuf::from(p),
                None => span::run_dir(&a.run_id).join(format!("otlp-{}.json", sanitize(&a.run_id))),
            };
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            match std::fs::write(&path, text) {
                Ok(()) => {
                    eprintln!(
                        "tracekit: exported {} span(s) → {}",
                        spans.len(),
                        path.display()
                    );
                    0
                }
                Err(e) => {
                    eprintln!("tracekit: writing {}: {e}", path.display());
                    1
                }
            }
        }
    }
}

/// List run-id directories under `base` that contain a `spans.jsonl`.
///
/// `Ok(None)` means `base` doesn't exist — a legitimate absence (no runs have
/// ever been recorded here). `Ok(Some(v))` is a completed listing (possibly
/// empty). `Err` means `base` (or an entry under it) exists but could not be
/// read (permission denied, IO) — a cannot-determine that must not be
/// reported the same way as "no runs" or a clean empty listing.
fn list_runs(base: &Path) -> Result<Option<Vec<String>>, String> {
    let entries = match std::fs::read_dir(base) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", base.display())),
    };
    let mut runs: Vec<String> = Vec::new();
    for entry in entries {
        match entry {
            Ok(e) if e.path().join("spans.jsonl").exists() => {
                if let Ok(name) = e.file_name().into_string() {
                    runs.push(name);
                }
            }
            Ok(_) => {}
            Err(e) => return Err(format!("entry under {}: {e}", base.display())),
        }
    }
    runs.sort();
    Ok(Some(runs))
}

fn cmd_list() -> i32 {
    let base = harness_core::config::base_dir("tracekit");
    match list_runs(&base) {
        Ok(None) => {
            eprintln!("tracekit: no runs recorded (no {})", base.display());
            0
        }
        Ok(Some(runs)) => {
            for r in &runs {
                println!("{r}");
            }
            0
        }
        Err(msg) => {
            // The dir (or an entry under it) EXISTS but couldn't be read:
            // cannot-determine, not "no runs" — must not report success on an
            // incomplete listing.
            eprintln!("tracekit: cannot list runs, unreadable: {msg}");
            1
        }
    }
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Mirror of span::sanitize for the default export filename.
fn sanitize(run_id: &str) -> String {
    run_id
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn missing_base_dir_is_ok_none() {
        let temp = TempDir::new().unwrap();
        let base = temp.path().join("does-not-exist");
        assert!(matches!(list_runs(&base), Ok(None)));
    }

    #[test]
    fn populated_base_dir_lists_runs_with_spans() {
        let temp = TempDir::new().unwrap();
        let base = temp.path().join("tracekit");
        std::fs::create_dir_all(base.join("run-a")).unwrap();
        std::fs::write(base.join("run-a").join("spans.jsonl"), "").unwrap();
        std::fs::create_dir_all(base.join("run-b-no-spans")).unwrap();

        let runs = list_runs(&base).unwrap().unwrap();
        assert_eq!(runs, vec!["run-a".to_string()]);
    }

    #[test]
    fn unreadable_existing_base_dir_is_err_not_ok_empty() {
        use std::os::unix::fs::PermissionsExt;
        let temp = TempDir::new().unwrap();
        let base = temp.path().join("tracekit");
        std::fs::create_dir_all(&base).unwrap();
        let mut perms = std::fs::metadata(&base).unwrap().permissions();
        perms.set_mode(0o000);
        std::fs::set_permissions(&base, perms.clone()).unwrap();

        let result = list_runs(&base);

        perms.set_mode(0o755);
        let _ = std::fs::set_permissions(&base, perms);

        assert!(
            result.is_err(),
            "an unreadable existing base dir must be Err, got {result:?}"
        );
    }
}
