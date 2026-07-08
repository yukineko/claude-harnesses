//! difflog — session diff-log for Claude Code.
//!
//! SessionStart: snapshot the HEAD SHA.
//! SessionEnd:   generate a structured git diff summary (commits, stat, files
//!               changed, bounded diff body) and write it to the log directory.
//!
//! A /difflog skill can then have an LLM read the log and produce a human-
//! readable narrative (89% vs 62% developer acceptance rate difference).

mod config;
mod git;
mod install;
mod record;
mod state;

use clap::{Parser, Subcommand};

use harness_core::hook::{read_stdin, read_stdin_if_piped, run_hook};

use config::Config;

#[derive(Parser)]
#[command(
    name = "difflog",
    version,
    about = "Session diff-log for Claude Code: snapshot HEAD at session start, write a structured git diff summary at session end."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// SessionStart hook: snapshot HEAD SHA.
    SessionStart,
    /// SessionEnd hook: write the diff-log.
    SessionEnd,
    /// List recent diff-logs.
    List {
        #[arg(long, default_value = "10")]
        limit: usize,
    },
    /// Print the contents of the most recent diff-log.
    Last,
    /// Merge SessionStart + SessionEnd hooks into ~/.claude/settings.json.
    Install {
        #[arg(long)]
        dry_run: bool,
    },
    /// Remove the hooks from ~/.claude/settings.json.
    Uninstall {
        #[arg(long)]
        dry_run: bool,
    },
    /// Write a starter ./difflog.toml.
    Init {
        #[arg(long)]
        force: bool,
    },
    /// Show the resolved config.
    Status,
}

fn main() {
    let cli = Cli::parse();
    match cli.command {
        Command::SessionStart => run_hook(session_start),
        Command::SessionEnd => run_hook(session_end),
        Command::List { limit } => list(limit),
        Command::Last => last(),
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

fn session_start() {
    if Config::disabled_env() {
        return;
    }
    let raw = read_stdin();
    let Some(input) = harness_core::hook::HookInput::parse(&raw) else {
        return;
    };
    if input.session_id.is_empty() {
        return;
    }
    let cwd = input.cwd_or_current();
    let cfg = Config::load(&cwd);
    if cfg.enabled {
        record::on_session_start(&input, &cfg);
    }
}

fn session_end() {
    if Config::disabled_env() {
        return;
    }
    let raw = read_stdin_if_piped();
    let Some(input) = harness_core::hook::HookInput::parse(&raw) else {
        return;
    };
    if input.session_id.is_empty() {
        return;
    }
    let cwd = input.cwd_or_current();
    let cfg = Config::load(&cwd);
    if cfg.enabled {
        record::on_session_end(&input, &cfg);
    }
}

fn list(limit: usize) {
    let cfg = Config::load(&std::env::current_dir().unwrap_or_else(|_| ".".into()));
    let dir = &cfg.log_dir;
    let Ok(rd) = std::fs::read_dir(dir) else {
        eprintln!("log_dir not found: {}", dir.display());
        return;
    };
    let mut entries: Vec<_> = rd
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map(|x| x == "md").unwrap_or(false))
        .collect();
    entries.sort_by_key(|e| std::cmp::Reverse(e.file_name()));
    for e in entries.iter().take(limit) {
        println!("{}", e.file_name().to_string_lossy());
    }
}

fn last() {
    let cfg = Config::load(&std::env::current_dir().unwrap_or_else(|_| ".".into()));
    let dir = &cfg.log_dir;
    let Ok(rd) = std::fs::read_dir(dir) else {
        eprintln!("log_dir not found: {}", dir.display());
        return;
    };
    let mut entries: Vec<_> = rd
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map(|x| x == "md").unwrap_or(false))
        .collect();
    entries.sort_by_key(|e| std::cmp::Reverse(e.file_name()));
    if let Some(e) = entries.first() {
        match std::fs::read_to_string(e.path()) {
            Ok(s) => print!("{s}"),
            Err(err) => eprintln!("error: {err}"),
        }
    } else {
        eprintln!("no diff-logs found in {}", dir.display());
    }
}

fn init(force: bool) -> anyhow::Result<()> {
    let path = std::path::PathBuf::from("difflog.toml");
    if path.exists() && !force {
        eprintln!("difflog.toml already exists — pass --force to overwrite");
        return Ok(());
    }
    let template = include_str!("../difflog.example.toml");
    std::fs::write(&path, template)?;
    eprintln!("wrote {}", path.display());
    Ok(())
}

fn status() {
    let cfg = Config::load(&std::env::current_dir().unwrap_or_else(|_| ".".into()));
    println!("enabled:          {}", cfg.enabled);
    println!("log_dir:          {}", cfg.log_dir.display());
    println!("diff_body_limit:  {} bytes", cfg.diff_body_limit);
    println!("exclude_globs:    {:?}", cfg.exclude_globs);
}
