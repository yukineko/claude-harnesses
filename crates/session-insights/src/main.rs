//! session-insights — per-session work metrics for Claude Code.
//!
//! `record` is the PostToolUse hook (counts each tool call); `stop` is the Stop
//! hook (counts a turn, and optionally writes a dated session note to an Obsidian
//! vault). `report` prints the rollup: a size class (XS–XL), a work category, the
//! turn/tool/file counts, and the top tools — the deterministic, no-API-key
//! analogue of Devin's Session Insights. Recording only ever writes to its own
//! state (and, opt-in, the vault); it never blocks a turn and always exits 0.

mod config;
mod context;
mod install;
mod metrics;
mod model;
mod obsidian;
mod record;

use std::path::Path;

use clap::{Parser, Subcommand};

use harness_core::hook::{read_stdin, read_stdin_if_piped, run_hook};

use config::Config;
use model::HookInput;

#[derive(Parser)]
#[command(
    name = "session-insights",
    version,
    about = "Per-session work metrics for Claude Code (PostToolUse + Stop hooks)."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// PostToolUse hook: record a tool call into the session rollup.
    Record,
    /// Stop hook: count a turn, optionally write the Obsidian session note.
    Stop,
    /// SessionEnd hook: optionally write/update the AEGIS-style record note.
    Sessionend,
    /// On-demand: (re)generate the record note for the current session and print
    /// its path. Backs the model-driven `/record` command (writes even when the
    /// automatic SessionEnd record is off; the vault must still exist).
    RecordNow {
        /// Session id (defaults to $CLAUDE_CODE_SESSION_ID).
        #[arg(long)]
        session: Option<String>,
    },
    /// Print session insights (latest by default).
    Report {
        /// Session id prefix to report on.
        #[arg(long)]
        session: Option<String>,
        /// Summarize all recorded sessions instead.
        #[arg(long)]
        all: bool,
        /// Also join context-governor's ledger health (saved/resident tokens,
        /// per-action counts) into the single-session report. Best-effort: shows
        /// "no context ledger" when the ledger is absent/empty.
        #[arg(long)]
        context: bool,
    },
    /// Merge the PostToolUse + Stop hooks into ~/.claude/settings.json.
    Install {
        #[arg(long)]
        dry_run: bool,
    },
    /// Remove the session-insights hooks from ~/.claude/settings.json.
    Uninstall {
        #[arg(long)]
        dry_run: bool,
    },
    /// Create the state dir.
    Init,
    /// Show the resolved config.
    Status,
}

fn main() {
    let cli = Cli::parse();
    match cli.command {
        Command::Record => run_hook(record),
        Command::Stop => run_hook(stop),
        Command::Sessionend => run_hook(sessionend),
        Command::RecordNow { session } => record_now(session),
        Command::Report {
            session,
            all,
            context,
        } => report(session, all, context),
        Command::Install { dry_run } => exit_on_err(install::install(dry_run)),
        Command::Uninstall { dry_run } => exit_on_err(install::uninstall(dry_run)),
        Command::Init => exit_on_err(init()),
        Command::Status => status(),
    }
}

fn exit_on_err(r: anyhow::Result<()>) {
    if let Err(e) = r {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn record() {
    if Config::disabled_env() {
        return;
    }
    let raw = read_stdin();
    let Some(input) = HookInput::parse(&raw) else {
        return;
    };
    let root = input.cwd_or_current();
    let cfg = Config::load(&root);
    if !cfg.enabled || input.tool_name.is_empty() || cfg.is_ignored(&input.tool_name) {
        return;
    }
    let session = input.session_key();
    let mut s = metrics::load(&cfg, &session);
    s.ensure(&session, &input.project_name(), &root.to_string_lossy());
    s.record_tool(&input.tool_name, input.target());
    metrics::save(&cfg, &session, &s);
}

fn stop() {
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
    let session = input.session_key();
    let mut s = metrics::load(&cfg, &session);
    s.ensure(&session, &input.project_name(), &root.to_string_lossy());
    s.record_turn();
    metrics::save(&cfg, &session, &s);
    obsidian::write_note(&cfg, &s);
    if cfg.record {
        let transcript_path = find_transcript(&s.session_id);
        record::write_from_session(&cfg, &s, &transcript_path, s.turns);
    }
}

/// SessionEnd hook: write/update the AEGIS-style record note. Uses the
/// harness-core HookInput because it carries `transcript_path` (the local
/// `model::HookInput` does not). No-op when disabled or no transcript/vault.
fn sessionend() {
    if Config::disabled_env() {
        return;
    }
    let raw = read_stdin_if_piped();
    let Some(input) = harness_core::hook::HookInput::parse(&raw) else {
        return;
    };
    let root = input.cwd_or_current();
    let cfg = Config::load(&root);
    if !cfg.record {
        return;
    }
    let session = if input.session_id.is_empty() {
        "_local".to_string()
    } else {
        input.session_id.clone()
    };
    // Pull the persisted rollup if present; fall back to transcript aggregation
    // for the project/turn data when it is absent.
    let mut s = metrics::load(&cfg, &session);
    if s.session_id.is_empty() {
        let project = root
            .file_name()
            .and_then(|p| p.to_str())
            .unwrap_or("project")
            .to_string();
        s.ensure(&session, &project, &root.to_string_lossy());
    }
    let turns_fallback = turns_fallback(&input.transcript_path);
    record::write_from_session(&cfg, &s, &input.transcript_path, turns_fallback);
}

/// Turn count from the transcript, used only when the ledger has none.
///
/// `0` here means "no transcript / no turns in it". It is deliberately NOT
/// where an unreadable sub-agent transcript dir lands silently: session-insights
/// is a passive recorder, so it keeps writing the note (a partial record beats
/// none), but an under-counted turn total is disclosed on stderr rather than
/// presented as the number. The cost figures in the same note carry the same
/// disclosure inline (see `record::cost_incompleteness`).
fn turns_fallback(transcript_path: &str) -> u64 {
    let Some(agg) = harness_core::usage::aggregate(transcript_path) else {
        return 0;
    };
    if let harness_core::verdict::Determination::Undetermined(why) = &agg.subagent_scan {
        eprintln!(
            "session-insights: sub-agent turns could not be read; the recorded \
             turn count is an under-count: {why}"
        );
    }
    agg.turns
}

/// Best-effort transcript discovery for a manually-run command (no hook stdin):
/// scan `~/.claude/projects/*/<session_id>.jsonl`. The cwd→project-key mapping
/// is not reliable, but the session id names the file. Returns "" when absent.
fn find_transcript(session_id: &str) -> String {
    if session_id.is_empty() {
        return String::new();
    }
    let projects = harness_core::config::home()
        .join(".claude")
        .join("projects");
    let Ok(entries) = std::fs::read_dir(&projects) else {
        return String::new();
    };
    let file = format!("{session_id}.jsonl");
    for e in entries.flatten() {
        let cand = e.path().join(&file);
        if cand.is_file() {
            return cand.to_string_lossy().into_owned();
        }
    }
    String::new()
}

/// On-demand record-note generation for the `/record` command. Resolves the
/// session id, discovers the transcript, (re)writes the note, prints its path.
/// Always exits 0 (the command layer handles the prose).
fn record_now(session_arg: Option<String>) {
    let root = std::env::current_dir().unwrap_or_else(|_| Path::new(".").to_path_buf());
    let mut cfg = Config::load(&root);
    // An explicit invocation writes even when the automatic SessionEnd record is
    // off; `write_record` still enforces that the vault directory exists.
    cfg.record = true;

    let session = session_arg
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("CLAUDE_CODE_SESSION_ID").ok())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "_local".to_string());

    let mut s = metrics::load(&cfg, &session);
    if s.session_id.is_empty() {
        let project = root
            .file_name()
            .and_then(|p| p.to_str())
            .unwrap_or("project")
            .to_string();
        s.ensure(&session, &project, &root.to_string_lossy());
    }
    let transcript = find_transcript(&session);
    let turns_fallback = turns_fallback(&transcript);
    match record::write_from_session(&cfg, &s, &transcript, turns_fallback) {
        Some(p) => println!("{}", p.display()),
        None => eprintln!(
            "no record note written (Obsidian vault not found: {})",
            cfg.obsidian_vault.display()
        ),
    }
}

fn report(session: Option<String>, all: bool, context: bool) {
    let root = std::env::current_dir().unwrap_or_else(|_| Path::new(".").to_path_buf());
    let cfg = Config::load(&root);
    if all {
        let sessions = metrics::load_all(&cfg);
        if sessions.is_empty() {
            println!("(no sessions recorded yet)");
            return;
        }
        for s in &sessions {
            println!(
                "{}  {:<14} {:>2} turns  {:>3} tools  size {:<2} {:<8} {}",
                metrics::short(&s.session_id),
                truncate(&s.project, 14),
                s.turns,
                s.tool_events,
                s.size(&cfg.size_thresholds),
                s.category(),
                s.last_at.chars().take(19).collect::<String>(),
            );
        }
        return;
    }
    let chosen = match session {
        Some(p) => metrics::find(&cfg, &p),
        None => metrics::latest(&cfg),
    };
    match chosen {
        Some(s) => {
            print!("{}", s.render_report(&cfg));
            // Loose-coupled join: read context-governor's ledger for this cwd and
            // merge a context-health block into the report. Best-effort — never
            // panics; an absent/empty ledger renders "no context ledger".
            if context {
                let summary = context::summarize(&root);
                print!("{}", context::render_block(&summary));
            }
        }
        None => println!("(no matching session — run some tools first, or try --all)"),
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n.saturating_sub(1)).collect::<String>() + "…"
    }
}

fn init() -> anyhow::Result<()> {
    let cfg = Config::default();
    std::fs::create_dir_all(&cfg.state_dir)?;
    println!("state dir ready: {}", cfg.state_dir.display());
    println!("Run `session-insights install` to wire the hooks, then `session-insights report`.");
    Ok(())
}

fn status() {
    let root = std::env::current_dir().unwrap_or_else(|_| Path::new(".").to_path_buf());
    let cfg = Config::load(&root);
    let src = if Config::project_path(&root).exists() {
        Config::project_path(&root)
    } else if Config::home_path().exists() {
        Config::home_path()
    } else {
        Path::new("(defaults — no config file)").to_path_buf()
    };
    println!("config:          {}", src.display());
    println!("enabled:         {}", cfg.enabled);
    println!("ignore_tools:    {}", cfg.ignore_tools.join(", "));
    println!("size_thresholds: {:?} (S,M,L,XL)", cfg.size_thresholds);
    println!("obsidian_log:    {}", cfg.obsidian_log);
    println!("obsidian_vault:  {}", cfg.obsidian_vault.display());
    println!("record:          {}", cfg.record);
    println!("record_dir:      {}", cfg.record_dir);
    println!("state_dir:       {}", cfg.state_dir.display());
    println!("sessions:        {}", metrics::load_all(&cfg).len());
}
