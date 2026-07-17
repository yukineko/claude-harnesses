mod config;
mod goal_link;
mod hypothesis;
mod install;
mod lock;
mod store;

mod hooks {
    pub mod session_start;
}

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use crate::hypothesis::{Criterion, Evidence, Risk};

/// Parse a `--measurement "metric=value"` argument into `(metric, value)`.
fn parse_measurement(s: &str) -> Result<(String, f64)> {
    let (metric, value) = s
        .split_once('=')
        .with_context(|| format!("measurement must be \"<metric>=<value>\": {s:?}"))?;
    let metric = metric.trim();
    if metric.is_empty() {
        anyhow::bail!("measurement is missing a metric name: {s:?}");
    }
    let value: f64 = value
        .trim()
        .parse()
        .with_context(|| format!("measurement value is not a number: {s:?}"))?;
    Ok((metric.to_string(), value))
}

#[derive(Parser)]
#[command(name = "hypothesis", about = "PDO hypothesis lifecycle management")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Add a new hypothesis
    Add {
        text: String,
        #[arg(long)]
        goal: Option<String>,
        /// Pre-registered success criterion, e.g. --success "activation >= 0.4"
        #[arg(long)]
        success: Option<String>,
        /// Pre-registered kill criterion, e.g. --kill "activation <= 0.2"
        #[arg(long)]
        kill: Option<String>,
        /// Discovery confidence (verification priority; higher = validated
        /// sooner). Omit to use the default (0.5).
        #[arg(long)]
        confidence: Option<f64>,
    },
    /// Mark a hypothesis as validated
    Validate {
        id: String,
        #[arg(long)]
        evidence: Vec<String>,
        /// Measured metric value, e.g. --measurement "activation=0.45". Required
        /// (and checked) when the hypothesis pre-registered a success criterion.
        #[arg(long)]
        measurement: Vec<String>,
        #[arg(long)]
        run: Option<String>,
    },
    /// Attach an assumption the hypothesis rests on (for RAT de-risking)
    Assume {
        id: String,
        #[arg(long)]
        text: String,
        /// Damage if false: low | medium | high
        #[arg(long)]
        risk: String,
        /// Evidence strength so far: strong | weak | none
        #[arg(long)]
        evidence: String,
    },
    /// Print the riskiest untested assumption (the leap of faith to de-risk first)
    Rat { id: String },
    /// Mark the assumption at <index> as tested (e.g. after a RAT)
    Tested { id: String, index: usize },
    /// Mark a hypothesis as awaiting measurement (deliverable shipped, not yet measured)
    AwaitMeasurement {
        id: String,
        #[arg(long)]
        run: Option<String>,
    },
    /// Mark a hypothesis as rejected
    Reject {
        id: String,
        #[arg(long)]
        reason: Option<String>,
        #[arg(long)]
        run: Option<String>,
    },
    /// Set a hypothesis's discovery confidence (verification priority)
    Confidence { id: String, value: f64 },
    /// List hypotheses
    List {
        /// Filter by lifecycle status: open | awaiting-measurement | validated |
        /// rejected. Omit to list all. An unrecognised value matches nothing.
        #[arg(long, value_name = "open|awaiting-measurement|validated|rejected")]
        status: Option<String>,
        /// Emit a JSON array instead of the plain-text listing. Each element
        /// has a `status` field using the same hyphenated vocabulary as
        /// `--status` (open|awaiting-measurement|validated|rejected), not
        /// serde's default snake_case rendering of `Status`.
        #[arg(long)]
        json: bool,
    },
    /// Shipped-vs-measured PDO health metrics (shipped/validated/rejected/
    /// awaiting counts + average measurement delay in days), as one JSON object
    Stats,
    /// Install SessionStart hook
    Install {
        #[arg(long)]
        dry_run: bool,
    },
    /// Uninstall SessionStart hook
    Uninstall,
    /// Run as SessionStart hook (internal)
    SessionStart,
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let cfg = config::Config::load()?;

    match cli.command {
        Command::Add {
            text,
            goal,
            success,
            kill,
            confidence,
        } => {
            let success = success.as_deref().map(Criterion::parse).transpose()?;
            let kill = kill.as_deref().map(Criterion::parse).transpose()?;
            let mut st = store::Store::load(&cfg)?;
            let id = st.add_with_criteria(text, goal, success, kill)?;
            // Default confidence (0.5) is set by `new()`; only override when given.
            if let Some(c) = confidence {
                st.set_confidence(&id, c)?;
            }
            println!("{id}");
        }
        Command::Validate {
            id,
            evidence,
            measurement,
            run,
        } => {
            let measurements = measurement
                .iter()
                .map(|m| parse_measurement(m))
                .collect::<Result<Vec<_>>>()?;
            let mut st = store::Store::load(&cfg)?;
            st.validate_with_measurements(&id, evidence, measurements, run)?;
        }
        Command::Assume {
            id,
            text,
            risk,
            evidence,
        } => {
            let risk = Risk::parse(&risk)?;
            let evidence = Evidence::parse(&evidence)?;
            let mut st = store::Store::load(&cfg)?;
            st.add_assumption(&id, text, risk, evidence)?;
            println!("{id} assumption recorded");
        }
        Command::Rat { id } => {
            let st = store::Store::load(&cfg)?;
            let h = st
                .all()
                .iter()
                .find(|h| h.id == id)
                .ok_or_else(|| anyhow::anyhow!("hypothesis not found: {id}"))?;
            // The riskiest untested leap of faith, if any. Prints
            // "<index>\t<assumption>" so flow can target it and later mark it
            // tested; prints nothing (exit 0) when the bet is already de-risked.
            if let Some(rat) = h.riskiest_assumption() {
                let index = h
                    .assumptions
                    .iter()
                    .position(|a| std::ptr::eq(a, rat))
                    .unwrap_or(0);
                println!("{index}\t{rat}");
            }
        }
        Command::Tested { id, index } => {
            let mut st = store::Store::load(&cfg)?;
            st.mark_assumption_tested(&id, index)?;
            println!("{id} assumption {index} marked tested");
        }
        Command::Confidence { id, value } => {
            let mut st = store::Store::load(&cfg)?;
            st.set_confidence(&id, value)?;
            println!("{id} confidence set to {value}");
        }
        Command::AwaitMeasurement { id, run } => {
            let mut st = store::Store::load(&cfg)?;
            st.mark_awaiting_measurement(&id, run)?;
            println!("{id} awaiting-measurement (shipped; run validate/reject after measuring)");
        }
        Command::Reject { id, reason, run } => {
            let mut st = store::Store::load(&cfg)?;
            st.reject(&id, reason, run)?;
        }
        Command::Stats => {
            let st = store::Store::load(&cfg)?;
            println!("{}", serde_json::to_string(&st.stats())?);
        }
        Command::List { status, json } => {
            let st = store::Store::load(&cfg)?;
            if json {
                let items: Vec<_> = st
                    .list(status.as_deref())
                    .into_iter()
                    .map(|h| {
                        serde_json::json!({
                            "id": h.id,
                            // Hyphenated Display string (e.g. "awaiting-measurement"),
                            // not serde's snake_case rendering of `Status`
                            // (which would emit "awaiting_measurement" with an
                            // underscore) — consumers like overwatch's
                            // bucket_hypotheses() match on this exact vocabulary.
                            "status": h.status.to_string(),
                            "text": h.text,
                            "confidence": h.confidence,
                            "run": h.condukt_run,
                        })
                    })
                    .collect();
                println!("{}", serde_json::to_string(&items)?);
                return Ok(());
            }
            for h in st.list(status.as_deref()) {
                let run_info = h
                    .condukt_run
                    .as_deref()
                    .map(|r| format!(" (run: {})", r))
                    .unwrap_or_default();
                let crit_info = match (&h.success_criterion, &h.kill_criterion) {
                    (None, None) => String::new(),
                    (s, k) => {
                        let mut parts = Vec::new();
                        if let Some(s) = s {
                            parts.push(format!("success: {s}"));
                        }
                        if let Some(k) = k {
                            parts.push(format!("kill: {k}"));
                        }
                        format!(" [{}]", parts.join(", "))
                    }
                };
                let rat_info = h
                    .riskiest_assumption()
                    .map(|a| format!(" [RAT: {}]", a.text))
                    .unwrap_or_default();
                println!(
                    "[{}] (conf {:.2}) {} — {}{}{}{}",
                    h.status, h.confidence, h.id, h.text, crit_info, rat_info, run_info
                );
            }
        }
        Command::Install { dry_run } => {
            install::install(dry_run)?;
        }
        Command::Uninstall => {
            install::uninstall()?;
        }
        Command::SessionStart => {
            harness_core::hook::run_hook(|| {
                if let Some(ctx) = hooks::session_start::run() {
                    println!("{}", serde_json::json!({ "additionalContext": ctx }));
                }
            });
        }
    }

    Ok(())
}

fn main() {
    if let Err(e) = run() {
        eprintln!("hypothesis: {e:#}");
        std::process::exit(1);
    }
}
