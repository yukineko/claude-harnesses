mod config;
mod install;
mod progress;
mod update;

use anyhow::Result;
use clap::{Parser, Subcommand};
use harness_core::hook::{read_stdin, run_hook, HookInput};
use harness_core::verdict::Determination;
use serde_json::json;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "taskprog", about = "Progress-file plugin for Claude Code")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// SessionStart hook: inject progress file as additionalContext
    SessionStart,
    /// Stop hook: prompt LLM to update the progress file
    Stop,
    /// Print the current progress file
    Show,
    /// Write content to the progress file (reads from stdin)
    Write {
        #[arg(long, default_value = ".")]
        cwd: String,
    },
    /// Init a starter taskprog.toml in the current directory
    Init {
        #[arg(default_value = "taskprog.toml")]
        target: String,
    },
    /// Merge hooks into ~/.claude/settings.json
    Install {
        #[arg(long)]
        dry_run: bool,
    },
    /// Remove taskprog hooks from ~/.claude/settings.json
    Uninstall {
        #[arg(long)]
        dry_run: bool,
    },
    /// Show resolved config
    Status,
}

/// The cwd for the non-hook subcommands, which act on the process's own
/// directory.
fn current_dir() -> String {
    std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| ".".to_string())
}

fn session_start_hook(input: &HookInput) -> Result<Option<String>> {
    if config::disabled_env() {
        return Ok(None);
    }
    let cwd = if input.cwd.is_empty() {
        "."
    } else {
        &input.cwd
    };
    let cfg = config::Config::load(cwd);
    if !cfg.enabled {
        return Ok(None);
    }
    match cfg.resolve_progress_path(cwd) {
        Determination::Known(path) => Ok(progress::build_context(&path, cfg.inject_limit)),
        // Say "I could not look" rather than injecting nothing — an empty
        // injection is read downstream as "there is no progress to know about",
        // which is a different claim (CLAUDE.md §1: silence is not a degrade).
        Determination::Undetermined(why) => Ok(Some(format!(
            "## Progress file\n\ntaskprog could not locate one: {why}. \
             No progress context was loaded for this session."
        ))),
    }
}

fn stop_hook(input: &HookInput) -> Result<()> {
    if config::disabled_env() {
        return Ok(());
    }
    let cwd = if input.cwd.is_empty() {
        "."
    } else {
        &input.cwd
    };
    let cfg = config::Config::load(cwd);
    if !cfg.enabled {
        return Ok(());
    }
    // The outcome (including `NotAnchored`) is reported on the model's channel
    // by `on_stop` itself; the Stop hook has no other channel and must not
    // change its exit status.
    update::on_stop(input, &cfg).map(|_| ())
}

/// Resolve the progress path for a user-facing subcommand, or print why it could
/// not be resolved and exit non-zero. Refusing to act must be loud: a silent
/// no-op here would be indistinguishable from success.
fn resolve_or_exit(cfg: &config::Config, cwd: &str) -> PathBuf {
    match cfg.resolve_progress_path(cwd) {
        Determination::Known(p) => p,
        Determination::Undetermined(why) => {
            eprintln!("taskprog: {why}");
            eprintln!("taskprog: set `progress_file` in taskprog.toml to name one explicitly.");
            std::process::exit(1);
        }
    }
}

fn main() {
    let cli = Cli::parse();
    match cli.command {
        Command::SessionStart => {
            run_hook(|| {
                let raw = read_stdin();
                let input = HookInput::parse(&raw).unwrap_or_default();
                if let Some(ctx) = session_start_hook(&input).unwrap_or(None) {
                    println!("{}", json!({ "additionalContext": ctx }));
                }
            });
        }
        Command::Stop => {
            run_hook(|| {
                let raw = read_stdin();
                let input = HookInput::parse(&raw).unwrap_or_default();
                let _ = stop_hook(&input);
            });
        }
        Command::Show => {
            let cwd = current_dir();
            let cfg = config::Config::load(&cwd);
            let path = resolve_or_exit(&cfg, &cwd);
            match progress::read_file(&path, 0) {
                Some(s) => print!("{s}"),
                None => eprintln!("No progress file at {}", path.display()),
            }
        }
        Command::Write { cwd } => {
            let cfg = config::Config::load(&cwd);
            let path = resolve_or_exit(&cfg, &cwd);
            let mut content = String::new();
            std::io::Read::read_to_string(&mut std::io::stdin(), &mut content)
                .expect("failed to read stdin");
            update::write_progress(&path, &content).expect("write failed");
            println!("wrote {}", path.display());
        }
        Command::Init { target } => {
            config::init_config(&target).unwrap_or_else(|e| eprintln!("Error: {e}"));
        }
        Command::Install { dry_run } => {
            install::install(dry_run).unwrap_or_else(|e| eprintln!("Error: {e}"));
        }
        Command::Uninstall { dry_run } => {
            install::uninstall(dry_run).unwrap_or_else(|e| eprintln!("Error: {e}"));
        }
        Command::Status => {
            let cwd = current_dir();
            let cfg = config::Config::load(&cwd);
            // `status` is a diagnostic: it reports the undetermined case as a
            // fact rather than exiting, because "where would it go?" is exactly
            // the question being asked.
            let (path, exists) = match cfg.resolve_progress_path(&cwd) {
                Determination::Known(p) => {
                    let e = p.exists();
                    (p.display().to_string(), e.to_string())
                }
                Determination::Undetermined(why) => {
                    (format!("(undetermined: {why})"), "no".to_string())
                }
            };
            println!("enabled:        {}", cfg.enabled);
            println!("progress_file:  {path}");
            println!("inject_limit:   {}", cfg.inject_limit);
            println!("file_exists:    {exists}");
        }
    }
}
