//! gauge — local LLMOps telemetry for Claude Code.
//!
//! One hook, one job: after every turn (Stop), re-read the session transcript
//! and record cumulative token usage, cache hits, tool calls, latency, and
//! estimated cost to a local store. `gauge report` then rolls that up by
//! project, model, and day. No API key, nothing leaves the machine.
//!
//! Like the rest of the toolkit the hook can only *observe*: `record` runs
//! under `run_hook`, which catches panics and always exits 0, so bad/empty
//! stdin, a missing transcript, or an unwritable store all silently record
//! nothing rather than break the turn.

mod config;
mod install;
mod model;
mod report;
mod store;

use std::path::Path;

use clap::{Parser, Subcommand};

use harness_core::hook::{read_stdin, run_hook};
use harness_core::{pricing, usage};

use config::Config;
use model::HookInput;
use store::SessionRecord;

#[derive(Parser)]
#[command(
    name = "gauge",
    version,
    about = "Local LLMOps telemetry for Claude Code (Stop hook + usage/cost reports)."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Stop hook: aggregate the current session's transcript into the store.
    Record,
    /// Print a usage + cost report across all recorded sessions, or emit JSON
    Report {
        /// Only sessions whose project name contains this string.
        #[arg(long)]
        project: Option<String>,
        /// Only sessions on/after this day (YYYY-MM-DD).
        #[arg(long)]
        since: Option<String>,
    },
    /// Show details for the most recent session (human) or emit machine-readable
    /// JSON (`--json`). With `--session <ID>` selects a specific session by id
    /// rather than the most-recently-updated one.
    Session {
        /// Emit JSON: `{session_id, cost_usd, models, agents}` instead of human text.
        #[arg(long)]
        json: bool,
        /// Session id to look up. Defaults to the most recently updated session.
        #[arg(long, value_name = "ID")]
        session: Option<String>,
    },
    /// List per-sub-agent cost for a session, read **live** from the sub-agent
    /// transcripts (`<session>/subagents/agent-<id>.jsonl`). Unlike `session`
    /// (which lumps every sub-agent into one bucket), this attributes cost to
    /// each `Task` invocation, so a caller can record per-task cost. Correlate
    /// on the `description` recorded in the `Task`'s sidecar.
    Subagents {
        /// Emit JSON: `[{agent_id, agent_type, description, cost_usd, turns}]`.
        #[arg(long)]
        json: bool,
        /// Session id whose sub-agents to list. Defaults to the newest transcript.
        #[arg(long, value_name = "ID")]
        session: Option<String>,
    },
    /// Merge the gauge Stop hook into ~/.claude/settings.json.
    Install {
        #[arg(long)]
        dry_run: bool,
    },
    /// Remove the gauge hook from ~/.claude/settings.json.
    Uninstall {
        #[arg(long)]
        dry_run: bool,
    },
    /// Write a starter ./gauge.toml.
    Init {
        #[arg(long)]
        force: bool,
    },
    /// Show the resolved config, store path, and recorded session count.
    Status,
}

fn main() {
    let cli = Cli::parse();
    match cli.command {
        Command::Record => run_hook(record_hook),
        Command::Report { project, since } => report_cmd(project, since),
        Command::Session { json, session } => session_cmd(json, session.as_deref()),
        Command::Subagents { json, session } => subagents_cmd(json, session.as_deref()),
        Command::Install { dry_run } => exit_on_err(install::install(dry_run)),
        Command::Uninstall { dry_run } => exit_on_err(install::uninstall(dry_run)),
        Command::Init { force } => exit_on_err(init(force)),
        Command::Status => status(),
    }
}

fn exit_on_err(r: anyhow::Result<()>) {
    if let Err(e) = r {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn record_hook() {
    if Config::disabled_env() {
        return;
    }
    let raw = read_stdin();
    let Some(input) = HookInput::parse(&raw) else {
        return;
    };
    let root = input.cwd_or_current();
    let cfg = Config::load(&root);
    if !cfg.enabled {
        return;
    }
    // Route the transcript through the single shared estimator so aggregation is
    // sourced identically across gauge/budgetguard/session-insights. Only the
    // aggregate is persisted here; the record's cost is priced on read.
    let Some(est) = harness_core::estimate_transcript_cost(&input.transcript_path, &cfg.pricing)
    else {
        return;
    };

    let rec = SessionRecord::from_aggregate(
        input.session_id.clone(),
        input.project_name(),
        root.to_string_lossy().to_string(),
        est.aggregate,
        cfg.track_tools,
        chrono::Local::now().to_rfc3339(),
    );
    store::upsert(&cfg.state_dir, &rec);
}

fn report_cmd(project: Option<String>, since: Option<String>) {
    let root = std::env::current_dir().unwrap_or_else(|_| Path::new(".").to_path_buf());
    let cfg = Config::load(&root);
    let mut records = store::load_all(&cfg.state_dir);
    if let Some(p) = project.as_deref() {
        let needle = p.to_lowercase();
        records.retain(|r| r.project.to_lowercase().contains(&needle));
    }
    if let Some(s) = since.as_deref() {
        records.retain(|r| r.day().map(|d| d.as_str() >= s).unwrap_or(false));
    }
    println!("{}", report::render(&records, &cfg.pricing));
}

fn session_cmd(json: bool, session_id: Option<&str>) {
    let root = std::env::current_dir().unwrap_or_else(|_| Path::new(".").to_path_buf());
    let cfg = Config::load(&root);

    let rec = if let Some(id) = session_id {
        store::load_one(cfg.state_dir.as_path(), id)
    } else {
        store::load_all(&cfg.state_dir)
            .into_iter()
            .max_by(|a, b| a.updated_at.cmp(&b.updated_at))
    };

    let Some(rec) = rec else {
        if json {
            println!("null");
        } else {
            println!("no sessions recorded yet.");
        }
        return;
    };

    if json {
        let cost_usd = pricing::session_cost(rec.models.iter(), &cfg.pricing);
        let agent_costs: std::collections::BTreeMap<String, serde_json::Value> = rec
            .agents
            .iter()
            .map(|(name, au)| {
                let c = pricing::session_cost(au.models.iter(), &cfg.pricing);
                (
                    name.clone(),
                    serde_json::json!({"cost_usd": c, "turns": au.turns}),
                )
            })
            .collect();
        let out = serde_json::json!({
            "session_id": rec.session_id,
            "cost_usd": cost_usd,
            "models": rec.models,
            "agents": agent_costs,
        });
        println!(
            "{}",
            serde_json::to_string(&out).unwrap_or_else(|_| "{}".to_string())
        );
        return;
    }

    let cost = pricing::session_cost(rec.models.iter(), &cfg.pricing);

    println!("session  {}", rec.session_id);
    println!("project  {} ({})", rec.project, rec.cwd);
    println!("turns    {}", rec.turns);
    println!(
        "cost     {}  ·  tokens {} ({})",
        report::money(cost),
        report::tokens_short(rec.total_tokens()),
        report::commas(rec.total_tokens()),
    );
    if let Some(dur) = duration_secs(&rec) {
        println!("duration {}", fmt_duration(dur));
    }

    println!("\nmodels");
    for (m, u) in &rec.models {
        println!(
            "  {:<24} {:>9}  in {} / out {} / cache r {} w {} ({} hit)",
            m,
            report::money(pricing::cost(m, u, &cfg.pricing)),
            report::tokens_short(u.input),
            report::tokens_short(u.output),
            report::tokens_short(u.cache_read),
            report::tokens_short(report::cache_write(u)),
            report::pct(report::cache_hit_rate(u)),
        );
    }

    if !rec.agents.is_empty() {
        println!("\nagents (main vs sub-agent)");
        let costs = rec.agent_costs(&cfg.pricing);
        let mut agents: Vec<_> = rec.agents.iter().collect();
        agents.sort_by_key(|b| std::cmp::Reverse(b.1.turns));
        for (name, a) in agents {
            println!(
                "  {:<12} {:>9}  {:>8}  {} turns",
                name,
                report::money(costs.get(name).copied().unwrap_or(0.0)),
                report::tokens_short(a.total_tokens()),
                a.turns,
            );
        }
    }

    if !rec.tools.is_empty() {
        println!("\ntools");
        let mut tools: Vec<_> = rec.tools.iter().collect();
        tools.sort_by(|a, b| b.1.cmp(a.1));
        for (name, count) in tools.iter().take(15) {
            println!("  {:<24} {}", name, count);
        }
    }
}

/// Resolve a session's main transcript file: `~/.claude/projects/*/<id>.jsonl`.
/// With no id, the most-recently-modified `*.jsonl` across all project dirs.
/// Globs the project dir (whose name is Claude's own path encoding) by matching
/// the session-id filename, so we never reimplement that encoding.
fn find_transcript(session_id: Option<&str>) -> Option<std::path::PathBuf> {
    let home = std::env::var_os("HOME")?;
    let projects = Path::new(&home).join(".claude").join("projects");
    let mut best: Option<(std::time::SystemTime, std::path::PathBuf)> = None;
    for proj in std::fs::read_dir(&projects).ok()?.flatten() {
        let dir = proj.path();
        if !dir.is_dir() {
            continue;
        }
        if let Some(id) = session_id {
            let cand = dir.join(format!("{id}.jsonl"));
            if cand.is_file() {
                return Some(cand);
            }
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for f in entries.flatten() {
            let p = f.path();
            if p.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let Ok(modified) = f.metadata().and_then(|md| md.modified()) else {
                continue;
            };
            if best.as_ref().map(|(t, _)| modified > *t).unwrap_or(true) {
                best = Some((modified, p));
            }
        }
    }
    best.map(|(_, p)| p)
}

fn subagents_cmd(json: bool, session_id: Option<&str>) {
    let root = std::env::current_dir().unwrap_or_else(|_| Path::new(".").to_path_buf());
    let cfg = Config::load(&root);
    let Some(path) = find_transcript(session_id) else {
        if json {
            println!("[]");
        } else {
            println!("no transcript found.");
        }
        return;
    };
    let subs = usage::subagent_usage(&path.to_string_lossy());
    let cost_of =
        |s: &usage::SubAgentUsage| -> f64 { pricing::session_cost(s.models.iter(), &cfg.pricing) };

    if json {
        let arr: Vec<serde_json::Value> = subs
            .iter()
            .map(|s| {
                serde_json::json!({
                    "agent_id": s.agent_id,
                    "agent_type": s.agent_type,
                    "description": s.description,
                    "cost_usd": cost_of(s),
                    "turns": s.turns,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string(&serde_json::Value::Array(arr)).unwrap_or_else(|_| "[]".into())
        );
        return;
    }

    if subs.is_empty() {
        println!("no sub-agents recorded for this session.");
        return;
    }
    println!(
        "sub-agents  {}",
        path.file_stem().and_then(|s| s.to_str()).unwrap_or("")
    );
    let mut subs = subs;
    subs.sort_by_key(|s| std::cmp::Reverse(s.turns));
    for s in &subs {
        println!(
            "  {:<18} {:>9}  {} turns  {}",
            s.agent_id,
            report::money(cost_of(s)),
            s.turns,
            s.description
                .as_deref()
                .or(s.agent_type.as_deref())
                .unwrap_or(""),
        );
    }
}

fn duration_secs(rec: &SessionRecord) -> Option<i64> {
    let first = rec.first_ts.as_ref()?;
    let last = rec.last_ts.as_ref()?;
    let a = chrono::DateTime::parse_from_rfc3339(first).ok()?;
    let b = chrono::DateTime::parse_from_rfc3339(last).ok()?;
    Some((b - a).num_seconds().max(0))
}

fn fmt_duration(secs: i64) -> String {
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if h > 0 {
        format!("{h}h {m}m {s}s")
    } else if m > 0 {
        format!("{m}m {s}s")
    } else {
        format!("{s}s")
    }
}

fn status() {
    let root = std::env::current_dir().unwrap_or_else(|_| Path::new(".").to_path_buf());
    let cfg = Config::load(&root);
    let records = store::load_all(&cfg.state_dir);
    let total_cost: f64 = records
        .iter()
        .map(|r| pricing::session_cost(r.models.iter(), &cfg.pricing))
        .sum();

    println!("config:       {}", Config::config_source(&root).display());
    println!("enabled:      {}", cfg.enabled);
    println!("track_tools:  {}", cfg.track_tools);
    println!("state_dir:    {}", cfg.state_dir.display());
    println!("sessions:     {}", records.len());
    println!("total cost:   {}", report::money(total_cost));
    println!(
        "pricing:      {}",
        if cfg.pricing.is_empty() {
            "built-in".to_string()
        } else {
            format!("{} override(s) + built-in", cfg.pricing.len())
        }
    );
    println!("\nrun `gauge report` for the full breakdown.");
}

const STARTER: &str = r#"# gauge.toml — local LLMOps telemetry for Claude Code.
#
# A Stop hook reads each session's transcript and records token usage, cache
# hits, tool calls, latency, and estimated cost to a local store. The hook only
# records — it never blocks a turn and never sends anything off the machine.
# Set GAUGE_DISABLE=1 to turn recording off entirely.

enabled = true
track_tools = true        # record per-tool call counts (Bash, Edit, …)

# state_dir = "~/.gauge/store"

# --- pricing overrides (optional) ---
# Built-in rates (USD per 1M tokens): Opus 5/25 · Sonnet 3/15 · Haiku 1/5 ·
# Fable 10/50. Cache writes bill at 1.25x input (5m) / 2x (1h); reads at 0.1x.
# `pattern` is matched as a substring against the model id; first match wins.
#
# [[pricing]]
# pattern = "opus"
# input = 5.0
# output = 25.0
"#;

fn init(force: bool) -> anyhow::Result<()> {
    let root = std::env::current_dir()?;
    let path = Config::project_path(&root);
    if path.exists() && !force {
        anyhow::bail!(
            "{} already exists (use --force to overwrite)",
            path.display()
        );
    }
    std::fs::write(&path, STARTER)?;
    println!("wrote {}", path.display());
    println!("Run `gauge install` to wire the Stop hook, then `gauge report` after some turns.");
    Ok(())
}
