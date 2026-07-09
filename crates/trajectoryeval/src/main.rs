//! trajectoryeval — a trajectory-match verifier, the sibling of an output verifier.
//!
//! condukt's online verifier checks a task's OUTPUT (its done_criteria);
//! trajectoryeval checks the PATH the worker took — its ordered tool-call
//! sequence — against an expected trajectory spec. Inspired by the trajectory
//! matchers in langchain-ai/agentevals.
//!
//! Exit codes (mirrors evalkit/schemaguard's 0/1/2 gate policy):
//!   0  — trajectory matched the spec (pass)
//!   1  — a deviation (missing / unexpected / out-of-order steps)
//!   2  — harness error (unreadable or unparseable input)
//!
//! This is a plain CLI gate, NOT a lifecycle hook — do not wrap in `run_hook`;
//! let real errors surface as exit 2.

mod extract;
mod match_traj;
mod tier;

use std::path::PathBuf;
use std::process::exit;

use clap::{Parser, Subcommand};

use match_traj::{evaluate, MatchResult, Spec};
use tier::{
    diff_snapshot, non_core_decision, DiffOutcome, NonCoreDecision, Tier, TierConfig, TierVerdict,
};

#[derive(Parser)]
#[command(
    name = "trajectoryeval",
    version,
    about = "Trajectory-match verifier: check an actual tool-call path against an expected spec."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Compare an actual ordered tool sequence against an expected trajectory spec.
    Check(CheckArgs),
    /// Stream a transcript and print its ordered tool_use names as a JSON array.
    Extract(ExtractArgs),
    /// Risk-tiered e2e verification: classify a flow core/non-core and diff it.
    Tier(TierArgs),
}

#[derive(clap::Args)]
struct CheckArgs {
    /// Path to the expected spec JSON ({mode, steps:[{tool,optional}]}).
    #[arg(long)]
    expected: PathBuf,
    /// Path to the actual trajectory JSON (an array of tool-name strings).
    #[arg(long)]
    actual: PathBuf,
    /// Emit the serialized MatchResult as JSON instead of a human report.
    #[arg(long)]
    json: bool,
}

#[derive(clap::Args)]
struct ExtractArgs {
    /// Path to the JSONL transcript to stream.
    #[arg(long)]
    transcript: PathBuf,
}

#[derive(clap::Args)]
struct TierArgs {
    /// Path to the tier config JSON ({core:[..], diff_strategy, sample_one_in}).
    #[arg(long)]
    config: PathBuf,
    /// The flow id to classify and verify.
    #[arg(long)]
    flow: String,
    /// Core flows: baseline snapshot JSON to diff against (required for core).
    #[arg(long)]
    baseline: Option<PathBuf>,
    /// Core flows: this run's captured snapshot JSON to diff.
    #[arg(long)]
    snapshot: Option<PathBuf>,
    /// Non-core flows: whether the flow exists (existence check). Takes an
    /// explicit bool, e.g. `--exists true` / `--exists false`. Defaults true.
    #[arg(long, action = clap::ArgAction::Set, default_value_t = true)]
    exists: bool,
    /// Non-core sampling seed (deterministic, seedable — no unseeded randomness).
    #[arg(long, default_value_t = 0)]
    seed: u64,
    /// Non-core sampling run index (which run this is, for seeded 1-in-N sampling).
    #[arg(long, default_value_t = 0)]
    run_index: u64,
    /// Emit the serialized TierVerdict as JSON instead of a human report.
    #[arg(long)]
    json: bool,
}

// ── command handlers ──────────────────────────────────────────────────────────

fn cmd_check(args: CheckArgs) -> i32 {
    // Read + parse the expected spec.
    let spec_raw = match std::fs::read_to_string(&args.expected) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "trajectoryeval: cannot read expected spec {}: {}",
                args.expected.display(),
                e
            );
            return 2;
        }
    };
    let spec: Spec = match serde_json::from_str(&spec_raw) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("trajectoryeval: invalid expected spec JSON: {}", e);
            return 2;
        }
    };

    // Read + parse the actual trajectory (array of tool-name strings).
    let actual_raw = match std::fs::read_to_string(&args.actual) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "trajectoryeval: cannot read actual trajectory {}: {}",
                args.actual.display(),
                e
            );
            return 2;
        }
    };
    let actual: Vec<String> = match serde_json::from_str(&actual_raw) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("trajectoryeval: invalid actual trajectory JSON (expected an array of tool-name strings): {}", e);
            return 2;
        }
    };

    let result = evaluate(&spec, &actual);

    if args.json {
        println!("{}", serde_json::to_string(&result).unwrap());
    } else {
        print_report(&result);
    }

    if result.pass {
        0
    } else {
        1
    }
}

fn print_report(r: &MatchResult) {
    if r.pass {
        println!("trajectory matched (pass)");
        return;
    }
    println!("trajectory deviated (fail)");
    if !r.missing.is_empty() {
        println!("  missing:     {}", r.missing.join(", "));
    }
    if !r.unexpected.is_empty() {
        println!("  unexpected:  {}", r.unexpected.join(", "));
    }
    if r.out_of_order {
        println!("  out of order: the right set of tools appeared in the wrong order");
    }
}

fn cmd_extract(args: ExtractArgs) -> i32 {
    match extract::extract_tools(&args.transcript) {
        Ok(tools) => {
            println!("{}", serde_json::to_string(&tools).unwrap());
            0
        }
        Err(e) => {
            eprintln!(
                "trajectoryeval: cannot read transcript {}: {}",
                args.transcript.display(),
                e
            );
            2
        }
    }
}

fn read_json(path: &std::path::Path, what: &str) -> Result<serde_json::Value, i32> {
    let raw = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "trajectoryeval: cannot read {} {}: {}",
                what,
                path.display(),
                e
            );
            return Err(2);
        }
    };
    match serde_json::from_str(&raw) {
        Ok(v) => Ok(v),
        Err(e) => {
            eprintln!(
                "trajectoryeval: invalid {} JSON in {}: {}",
                what,
                path.display(),
                e
            );
            Err(2)
        }
    }
}

fn cmd_tier(args: TierArgs) -> i32 {
    // Read + parse the tier config.
    let cfg_raw = match std::fs::read_to_string(&args.config) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "trajectoryeval: cannot read tier config {}: {}",
                args.config.display(),
                e
            );
            return 2;
        }
    };
    let cfg: TierConfig = match serde_json::from_str(&cfg_raw) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("trajectoryeval: invalid tier config JSON: {}", e);
            return 2;
        }
    };

    let tier = cfg.tier_of(&args.flow);

    let (verdict, code) = match tier {
        Tier::Core => {
            // Core flows: capture-and-diff every run. Baseline + snapshot required.
            let (baseline, snapshot) = match (&args.baseline, &args.snapshot) {
                (Some(b), Some(s)) => (b, s),
                _ => {
                    eprintln!(
                        "trajectoryeval: core flow '{}' requires --baseline and --snapshot",
                        args.flow
                    );
                    return 2;
                }
            };
            let baseline = match read_json(baseline, "baseline") {
                Ok(v) => v,
                Err(c) => return c,
            };
            let snapshot = match read_json(snapshot, "snapshot") {
                Ok(v) => v,
                Err(c) => return c,
            };
            let diff = diff_snapshot(cfg.diff_strategy, &baseline, &snapshot);
            let pass = diff.is_match();
            (
                TierVerdict {
                    flow_id: args.flow.clone(),
                    tier,
                    diff: Some(diff),
                    non_core: None,
                    pass,
                },
                if pass { 0 } else { 1 },
            )
        }
        Tier::NonCore => {
            let decision = non_core_decision(
                &args.flow,
                args.exists,
                cfg.sample_one_in,
                args.seed,
                args.run_index,
            );
            // Absent (existence check failed) is a deviation; present is a pass.
            let pass = !matches!(decision, NonCoreDecision::Absent);
            (
                TierVerdict {
                    flow_id: args.flow.clone(),
                    tier,
                    diff: None,
                    non_core: Some(decision),
                    pass,
                },
                if pass { 0 } else { 1 },
            )
        }
    };

    if args.json {
        println!("{}", serde_json::to_string(&verdict).unwrap());
    } else {
        print_tier_report(&verdict);
    }
    code
}

fn print_tier_report(v: &TierVerdict) {
    let tier = match v.tier {
        Tier::Core => "core",
        Tier::NonCore => "non-core",
    };
    println!("flow '{}' — tier: {}", v.flow_id, tier);
    if let Some(diff) = &v.diff {
        match diff {
            DiffOutcome::Match => println!("  diff: match (snapshot equals baseline)"),
            DiffOutcome::Mismatch { paths } => {
                println!("  diff: MISMATCH at {}", paths.join(", "));
            }
            DiffOutcome::Stubbed { .. } => {
                println!("  diff: screenshot/perceptual-hash strategy is a stub (not implemented)");
            }
        }
    }
    if let Some(nc) = &v.non_core {
        match nc {
            NonCoreDecision::Absent => println!("  existence: ABSENT (flow not present)"),
            NonCoreDecision::ExistsSkipped => {
                println!("  existence: present (not sampled this run)")
            }
            NonCoreDecision::ExistsSampled => println!("  existence: present (sampled this run)"),
        }
    }
    println!("  result: {}", if v.pass { "pass" } else { "fail" });
}

// ── entry point ───────────────────────────────────────────────────────────────

fn main() {
    let cli = Cli::parse();
    let code = match cli.command {
        Command::Check(args) => cmd_check(args),
        Command::Extract(args) => cmd_extract(args),
        Command::Tier(args) => cmd_tier(args),
    };
    exit(code);
}
