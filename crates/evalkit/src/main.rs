//! evalkit — offline golden-regression eval harness for Claude Code plugins.
//!
//! `evalkit run` discovers `evals/*.jsonl` golden cases and asserts over each
//! subject — a `file`'s contents (catch a SKILL.md/prompt edit that drops an
//! invariant) or a `cmd`'s stdout+exit (catch a CLI contract regression). It is
//! the *offline* sibling of condukt's online Phase-6 verifier: deterministic and
//! API-key-free. No CI workflow and no hook runs it (pinned by
//! `tests/prose_claims.rs::claim_no_ci_no_hook`); it is invoked by hand.
//!
//! `run` exit codes: `0` all passed, `1` a real regression, `2` harness error
//! (no cases / only draft cases / unreadable eval files) — so a caller can tell
//! a regression from a misconfigured path. `canary` exits `1` by default when a
//! golden's verdict was lost (regression / dropped / pass|fail→draft; `--report-only`
//! makes that `0`) and `2` when either side is unreadable, empty, or all-draft.
//!
//! This is a plain CLI, not a lifecycle hook, so it does not wrap work in
//! `run_hook`: a gate that swallowed its own failures and exited 0 would be
//! worse than useless.

mod canary;
mod case;
mod run;

use std::path::PathBuf;
use std::process::exit;

use clap::{Args, Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "evalkit",
    version,
    about = "Offline golden-regression eval harness (prompt + CLI-contract invariants)."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run golden cases; exit 1 if any assertion fails, 2 on harness error.
    Run(RunArgs),
    /// List discovered cases without running them.
    List(RunArgs),
    /// Diff two `evalkit run --json` outputs (old vs new): pass-rate delta plus
    /// regressions / fixes / added / dropped / demoted — the offline promptfoo
    /// side-by-side. Exits 1 when a golden's verdict was lost unless --report-only.
    Canary(CanaryArgs),
}

#[derive(Args)]
struct RunArgs {
    /// Directory holding `*.jsonl` golden case files (relative to --root).
    #[arg(long, default_value = "evals")]
    dir: PathBuf,
    /// Prepend this dir to PATH when running `cmd` cases (e.g. target/release).
    #[arg(long)]
    bin_dir: Option<PathBuf>,
    /// Project root that `file` paths and the `--dir` resolve against (default: CWD).
    #[arg(long)]
    root: Option<PathBuf>,
    /// Emit a machine-readable JSON summary instead of the human report.
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
struct CanaryArgs {
    /// Baseline `evalkit run --json` output (the old prompt/SKILL version).
    #[arg(long)]
    baseline: PathBuf,
    /// Current `evalkit run --json` output (the new prompt/SKILL version).
    #[arg(long)]
    current: PathBuf,
    /// Emit a machine-readable JSON summary instead of the human report.
    #[arg(long)]
    json: bool,
    /// Report only: exit 0 even when a golden's verdict was lost (regression,
    /// dropped, or pass/fail → draft). Without it, any such loss exits 1. An empty
    /// or all-draft side still exits 2.
    #[arg(long)]
    report_only: bool,
}

fn main() {
    let cli = Cli::parse();
    let code = match cli.command {
        Command::Run(a) => run::execute(a.dir, a.bin_dir, a.root, a.json, false),
        Command::List(a) => run::execute(a.dir, a.bin_dir, a.root, a.json, true),
        Command::Canary(a) => canary::execute(a.baseline, a.current, a.json, a.report_only),
    };
    exit(code);
}
