//! replaykit — a trace→golden replay regression harness, the sibling of curate.
//!
//! curate promotes a fugu-router *playbook* into an evalkit golden; replaykit
//! promotes a tracekit-recorded *run trace* into one. It distils a run's spans
//! into a portable trajectory summary (ordered steps + a pinned `expect` block
//! of phase set / error count / cost) and emits a golden whose `cmd` re-checks
//! those invariants — so a regression surfaces as a failing golden when the
//! golden is run.
//!
//! Exit codes (mirrors evalkit/trajectoryeval's 0/1/2 gate policy):
//!   0  — replay matched the pinned invariants (pass)
//!   1  — a real regression / invariant violation
//!   2  — harness error (missing / unreadable / malformed input)
//!
//! This is a plain CLI gate, NOT a lifecycle hook — let real errors surface as
//! exit 2.

mod promote;
mod summary;
mod trace;
mod verify;

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::exit;

use clap::{Args, Parser, Subcommand};
use serde_json::Value;

use summary::TrajectorySummary;

#[derive(Parser)]
#[command(
    name = "replaykit",
    version,
    about = "Trace→golden replay harness: distil tracekit run traces into evalkit golden replay cases."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Distil a run's spans into a portable trajectory summary (pretty JSON).
    Extract(ExtractArgs),
    /// Recompute a committed summary fixture's aggregates and check its `expect`.
    Verify(VerifyArgs),
    /// Promote a run into a committed golden replay dataset (fixture + golden).
    Promote(PromoteArgs),
}

#[derive(Args)]
struct ExtractArgs {
    /// Run id to extract (resolves the default spans path).
    #[arg(long)]
    run: String,
    /// Override the spans.jsonl path (default ~/.tracekit/<sanitize(run)>/spans.jsonl).
    #[arg(long)]
    spans: Option<PathBuf>,
    /// Where to write the summary JSON (`-` = stdout, the default).
    #[arg(long, default_value = "-")]
    out: String,
}

#[derive(Args)]
struct VerifyArgs {
    /// Path to a committed summary fixture JSON.
    fixture: PathBuf,
}

#[derive(Args)]
struct PromoteArgs {
    /// Run id to promote (resolves the default spans path + the case id).
    #[arg(long)]
    run: String,
    /// Override the spans.jsonl path (default ~/.tracekit/<sanitize(run)>/spans.jsonl).
    #[arg(long)]
    spans: Option<PathBuf>,
    /// Project root the dataset path resolves against (default: CWD).
    #[arg(long)]
    root: Option<PathBuf>,
    /// Eval dir under root that holds replay/ (default: evals).
    #[arg(long, default_value = "evals")]
    evals_dir: PathBuf,
    /// Dataset name → <root>/<evals_dir>/replay/<name>.jsonl.
    #[arg(long, default_value = "replayed")]
    dataset: String,
    /// Reserved: mark the emitted golden as a draft (currently a no-op flag,
    /// kept for parity with curate's review-before-trust workflow).
    #[arg(long)]
    draft: bool,
}

fn main() {
    let cli = Cli::parse();
    let code = match cli.command {
        Command::Extract(a) => cmd_extract(a),
        Command::Verify(a) => cmd_verify(a),
        Command::Promote(a) => cmd_promote(a),
    };
    exit(code);
}

// ── extract ────────────────────────────────────────────────────────────────────

fn cmd_extract(a: ExtractArgs) -> i32 {
    let spans_path = a.spans.unwrap_or_else(|| default_spans_path(&a.run));
    let (spans, skipped) = match trace::load_spans(&spans_path) {
        Ok(v) => v,
        Err(e) => {
            eprintln!(
                "replaykit: cannot read spans {}: {}",
                spans_path.display(),
                e
            );
            return 2;
        }
    };
    if skipped > 0 {
        eprintln!("replaykit: skipped {skipped} malformed span line(s)");
    }
    let error_spans = spans.iter().filter(|s| s.is_error()).count();
    let summary = TrajectorySummary::from_spans(&a.run, &spans);
    eprintln!(
        "replaykit: {} span(s), {error_spans} error(s), {}ms total",
        summary.steps.len(),
        summary.total_ms()
    );
    let json = match serde_json::to_string_pretty(&summary) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("replaykit: cannot serialize summary: {e}");
            return 2;
        }
    };
    if a.out == "-" {
        println!("{json}");
    } else if let Err(e) = std::fs::write(&a.out, format!("{json}\n")) {
        eprintln!("replaykit: cannot write {}: {}", a.out, e);
        return 2;
    }
    0
}

// ── verify ─────────────────────────────────────────────────────────────────────

fn cmd_verify(a: VerifyArgs) -> i32 {
    let raw = match std::fs::read_to_string(&a.fixture) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "replaykit: cannot read fixture {}: {}",
                a.fixture.display(),
                e
            );
            return 2;
        }
    };
    let summary: TrajectorySummary = match serde_json::from_str(&raw) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "replaykit: invalid summary fixture {}: {}",
                a.fixture.display(),
                e
            );
            return 2;
        }
    };
    match verify::verify(&summary) {
        Ok(()) => {
            println!("replaykit: replay matched (pass) — {}", summary.run_id);
            0
        }
        Err(violations) => {
            eprintln!("replaykit: replay drifted (fail) — {}", summary.run_id);
            for v in &violations {
                eprintln!("  - {v}");
            }
            1
        }
    }
}

// ── promote ────────────────────────────────────────────────────────────────────

fn cmd_promote(a: PromoteArgs) -> i32 {
    let _ = a.draft; // reserved for parity with curate; currently a no-op.
    let spans_path = a.spans.unwrap_or_else(|| default_spans_path(&a.run));
    let (spans, skipped) = match trace::load_spans(&spans_path) {
        Ok(v) => v,
        Err(e) => {
            eprintln!(
                "replaykit: cannot read spans {}: {}",
                spans_path.display(),
                e
            );
            return 2;
        }
    };
    if skipped > 0 {
        eprintln!("replaykit: skipped {skipped} malformed span line(s)");
    }
    let error_spans = spans.iter().filter(|s| s.is_error()).count();
    let summary = TrajectorySummary::from_spans(&a.run, &spans);
    eprintln!(
        "replaykit: {} span(s), {error_spans} error(s), {}ms total",
        summary.steps.len(),
        summary.total_ms()
    );
    let id = promote::slug_id(&a.run);

    let root = a.root.unwrap_or_else(|| PathBuf::from("."));
    let replay_dir = root.join(&a.evals_dir).join("replay");

    // The golden references the fixture by a path RELATIVE to root, so the
    // committed golden travels with the repo regardless of where root sits.
    let fixture_rel = a
        .evals_dir
        .join("replay")
        .join("fixtures")
        .join(format!("{id}.json"));
    let fixture_abs = replay_dir.join("fixtures").join(format!("{id}.json"));
    let golden = promote::derive_golden(&a.run, &fixture_rel.to_string_lossy());

    let dataset = replay_dir.join(format!("{}.jsonl", sanitize(&a.dataset)));

    if existing_ids(&dataset).contains(&id) {
        eprintln!(
            "replaykit: \"{}\" already promoted (id {id}) in {} — skipping",
            a.run,
            dataset.display()
        );
        return 0;
    }

    // Write the portable summary fixture (pretty).
    if let Err(e) = write_fixture(&fixture_abs, &summary) {
        eprintln!("replaykit: writing {}: {}", fixture_abs.display(), e);
        return 2;
    }
    // Append the golden line (dedup already checked).
    if let Err(e) = append_golden(&dataset, &golden) {
        eprintln!("replaykit: writing {}: {}", dataset.display(), e);
        return 2;
    }
    println!(
        "replaykit: promoted \"{}\" → {} (fixture {})",
        a.run,
        dataset.display(),
        fixture_abs.display()
    );
    0
}

// ── path + io helpers ───────────────────────────────────────────────────────────

/// Default spans path: `~/.tracekit/<sanitize(run_id)>/spans.jsonl`. tracekit
/// sanitizes the run id the same way for its on-disk dir.
fn default_spans_path(run_id: &str) -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home)
        .join(".tracekit")
        .join(sanitize(run_id))
        .join("spans.jsonl")
}

/// Keep a name filesystem-safe: every char not in [A-Za-z0-9_-] → '_'.
fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Case ids already present in a dataset (for dedup). Missing file → empty.
/// Blank lines and `//` comment lines are skipped.
fn existing_ids(path: &Path) -> HashSet<String> {
    let mut ids = HashSet::new();
    let Ok(text) = std::fs::read_to_string(path) else {
        return ids;
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("//") {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<Value>(line) {
            if let Some(id) = v.get("id").and_then(|x| x.as_str()) {
                ids.insert(id.to_string());
            }
        }
    }
    ids
}

/// Write a summary fixture as pretty JSON, creating its dir on first use.
fn write_fixture(path: &Path, summary: &TrajectorySummary) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let json = serde_json::to_string_pretty(summary)?;
    std::fs::write(path, format!("{json}\n"))
}

/// Append one golden case as a JSON line, creating the replay dir on first use.
fn append_golden(path: &Path, golden: &Value) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(f, "{}", serde_json::to_string(golden)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_replaces_unsafe_chars() {
        assert_eq!(sanitize("run-2026_06"), "run-2026_06");
        assert_eq!(sanitize("a/b c:d"), "a_b_c_d");
        assert_eq!(sanitize("実行"), "__"); // two non-ascii chars → two underscores
    }

    #[test]
    fn default_spans_path_uses_sanitized_run_id() {
        // HOME is set in the test env; just assert structure.
        let p = default_spans_path("my run/id");
        let s = p.to_string_lossy();
        assert!(s.ends_with("/.tracekit/my_run_id/spans.jsonl"), "{s}");
    }

    #[test]
    fn existing_ids_collects_and_skips_comments() {
        let dir = std::env::temp_dir().join(format!("replaykit-ids-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("d.jsonl");
        std::fs::write(
            &p,
            "// comment\n{\"id\":\"a\",\"cmd\":[\"replaykit\"]}\n\n{\"id\":\"b\",\"cmd\":[\"x\"]}\n",
        )
        .unwrap();
        let ids = existing_ids(&p);
        assert!(ids.contains("a") && ids.contains("b"));
        assert_eq!(ids.len(), 2);
        std::fs::remove_dir_all(&dir).ok();
    }
}
