//! benchkit CLI — SWE-bench Verified runner skeleton.
//!
//! `benchkit load` reads a JSONL split into typed instances (deterministic,
//! offline). `benchkit download` fetches the upstream dataset via `curl` into a
//! local cache — the only subcommand that touches the network, and only when
//! explicitly invoked.

use std::path::PathBuf;
use std::process::exit;

use clap::{Parser, Subcommand};

use benchkit::auditsample;
use benchkit::download::{self, Outcome};
use benchkit::harness::{self, PatchGenerator};
use benchkit::loader;
use benchkit::model::Instance;

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
    /// Run one instance through the harness (setup -> generate -> test -> score).
    ///
    /// Without `--real` this is a **dry run**: it wires the pure harness with a
    /// stub generator and empty results (no external effects). With `--real` it
    /// dispatches to the explicitly-gated real path that shells out to git /
    /// pytest — the only way that path is ever reached.
    RunInstance {
        /// Path to a JSONL file containing the instance(s).
        path: PathBuf,
        /// Instance id to run.
        instance_id: String,
        /// Run the REAL exec path (git clone + git apply + pytest; network + shell).
        #[arg(long)]
        real: bool,
    },
    /// Post-hoc sampling calibration loop over auto-gate-passed changes.
    ///
    /// Reads a JSONL of changes that passed only via auto-gates, draws a
    /// deterministic seeded sample, and (when audit verdicts are supplied)
    /// routes the misses into two feedback paths: new-invariant candidates and
    /// a threshold-adjustment ratify queue (never auto-applied). Pure and
    /// hermetic — no LLM in the decision path, no clock in the sampling.
    AuditSample {
        /// JSONL of gate-passed changes (one GatePassedChange per line).
        changes: PathBuf,
        /// Optional JSONL of stricter-audit verdicts (one AuditResult per line).
        #[arg(long)]
        audits: Option<PathBuf>,
        /// Fraction of the population to sample, in [0.0, 1.0] (default 0.1).
        #[arg(long, default_value_t = 0.1)]
        fraction: f64,
        /// PRNG seed for reproducible sampling (deterministic; required for
        /// hermetic runs — never derived from the clock).
        #[arg(long, default_value_t = 0)]
        seed: u64,
        /// Emit a machine-readable JSON report instead of the human summary.
        #[arg(long)]
        json: bool,
    },
}

/// CLI-side placeholder generator. The real model/LLM seam is not wired in this
/// slice; this returns an empty candidate so the dry path exercises the
/// plumbing, and the real path's [`harness::RealExecSource`] is what gates
/// external effects.
struct StubGenerator;

impl PatchGenerator for StubGenerator {
    fn generate(&self, _instance: &Instance) -> anyhow::Result<String> {
        Ok(String::new())
    }
}

/// Dry-run result source: yields no results, so scoring is fail-closed
/// (unresolved). Used by `run-instance` without `--real`; performs no I/O.
struct DrySource;

impl harness::TestResultSource for DrySource {
    fn results(
        &self,
        _instance: &Instance,
        _candidate_patch: &str,
    ) -> anyhow::Result<std::collections::BTreeMap<String, bool>> {
        Ok(std::collections::BTreeMap::new())
    }
}

/// Look up a single instance by id from a JSONL file.
fn find_instance(path: &PathBuf, instance_id: &str) -> anyhow::Result<Instance> {
    let instances = loader::load_instances(path)?;
    instances
        .into_iter()
        .find(|i| i.instance_id == instance_id)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "instance id {instance_id:?} not found in {}",
                path.display()
            )
        })
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
        Command::RunInstance {
            path,
            instance_id,
            real,
        } => match find_instance(&path, &instance_id) {
            Ok(instance) => {
                let generator = StubGenerator;
                let result = if real {
                    // Explicitly-gated real path: shells out to git / pytest.
                    harness::run_instance_real(&instance, &generator)
                } else {
                    // Dry path: pure harness, no external effects.
                    harness::run_instance(&instance, &generator, &DrySource)
                };
                match result {
                    Ok(verdict) => {
                        println!(
                            "{}: resolved={} ({} test results)",
                            verdict.instance_id,
                            verdict.resolved,
                            verdict.results.len()
                        );
                        0
                    }
                    Err(e) => {
                        eprintln!("error: {e:#}");
                        1
                    }
                }
            }
            Err(e) => {
                eprintln!("error: {e:#}");
                1
            }
        },
        Command::AuditSample {
            changes,
            audits,
            fraction,
            seed,
            json,
        } => auditsample::execute(&changes, audits.as_deref(), fraction, seed, json),
    };
    exit(code);
}
