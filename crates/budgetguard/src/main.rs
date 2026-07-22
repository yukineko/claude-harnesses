//! budgetguard — real-time cost budget gate for Claude Code.
//!
//! On every Stop it reads the session transcript, computes the USD cost using
//! the same pricing table as gauge, and blocks the turn if session or daily
//! limits are exceeded. Harness errors always exit 0 (never break the turn).
#![deny(clippy::panic)]

mod config;
mod gate;
mod install;
mod lock;

use clap::{Args, Parser, Subcommand};

use harness_core::hook::read_stdin;

use config::Config;

#[derive(Parser)]
#[command(
    name = "budgetguard",
    version,
    about = "Real-time cost budget gate for Claude Code (Stop hook + spend reports)."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Stop hook: check session and daily cost against configured limits.
    Gate,
    /// Merge the budgetguard Stop hook into ~/.claude/settings.json.
    Install {
        #[arg(long)]
        dry_run: bool,
    },
    /// Remove the budgetguard Stop hook from ~/.claude/settings.json.
    Uninstall {
        #[arg(long)]
        dry_run: bool,
    },
    /// Write a starter ./budgetguard.toml.
    Init {
        #[arg(long)]
        force: bool,
    },
    /// Show the resolved config and today's spend.
    Status(StatusArgs),
}

#[derive(Args)]
struct StatusArgs {
    /// Emit machine-readable JSON ({day_usd, daily_warn_usd, daily_block_usd,
    /// pressure}) instead of the human table. Lets a downstream router (e.g.
    /// fugu-router) read budget pressure and downgrade model choices.
    #[arg(long)]
    json: bool,
}

fn main() {
    let cli = Cli::parse();
    match cli.command {
        Command::Gate => gate_command(),
        Command::Install { dry_run } => exit_on_err(install::install(dry_run)),
        Command::Uninstall { dry_run } => exit_on_err(install::uninstall(dry_run)),
        Command::Init { force } => exit_on_err(init(force)),
        Command::Status(args) => status(args),
    }
}

fn exit_on_err(r: anyhow::Result<()>) {
    if let Err(e) = r {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

/// The Stop hook entry point. Reads stdin once and determines the panic-guard
/// mode flags up front, then runs the actual gate logic under
/// `harness_core::gate::run::run_guarded`: a panic in `gate_run` (a crash
/// before any Verdict was decided) now fails CLOSED — a `decision:block` is
/// emitted — instead of the old behavior of silently exiting 0 with no
/// decision (indistinguishable from a passing gate). `disabled_env` is checked
/// first, outside the guard, so the operator's escape hatch stays reachable
/// even if `gate_run` were to crash deterministically.
fn gate_command() -> ! {
    if Config::disabled_env() {
        std::process::exit(0);
    }
    let raw = read_stdin();
    let hook = harness_core::hook::HookInput::parse(&raw);
    let interactive = hook.is_none();
    let stop_hook_active = hook.as_ref().is_some_and(|h| h.stop_hook_active);
    harness_core::gate::run::run_guarded("budgetguard", interactive, stop_hook_active, move || {
        gate_run(hook)
    });
    std::process::exit(0);
}

fn gate_run(hook: Option<harness_core::hook::HookInput>) {
    let Some(input) = hook else {
        return;
    };
    if input.transcript_path.is_empty() || input.session_id.is_empty() {
        return;
    }
    // Recursion guard: when this Stop fires because a previous Stop hook already
    // blocked (`stop_hook_active`), do not block again. An over-budget session
    // would otherwise re-block on every re-entry and trap the turn — the user
    // could never end it. They have already been warned once; allow them to stop.
    if input.stop_hook_active {
        return;
    }

    let cwd = input.cwd_or_current();
    let cfg = Config::load(&cwd);
    if !cfg.enabled {
        return;
    }

    let today = today_str();
    let result = gate::evaluate(&cfg, &input.session_id, &input.transcript_path, &today);
    gate::emit_and_exit(result);
}

fn today_str() -> String {
    // Daily budget resets at LOCAL midnight (matching session-insights' daily key),
    // not UTC midnight — otherwise the reset rolls over at 09:00 for +09:00 (JST) users
    // instead of local midnight.
    date_key(chrono::Local::now())
}

/// Format the daily-reset key (`YYYY-MM-DD`) from a timezone-aware instant.
/// Pure + tz-generic so the local-vs-UTC calendar boundary is unit-testable.
fn date_key<Tz: chrono::TimeZone>(now: chrono::DateTime<Tz>) -> String
where
    Tz::Offset: std::fmt::Display,
{
    now.format("%Y-%m-%d").to_string()
}

fn init(force: bool) -> anyhow::Result<()> {
    let path = std::path::PathBuf::from("budgetguard.toml");
    if path.exists() && !force {
        eprintln!("budgetguard.toml already exists — pass --force to overwrite");
        return Ok(());
    }
    let template = include_str!("../budgetguard.example.toml");
    std::fs::write(&path, template)?;
    eprintln!("wrote {}", path.display());
    Ok(())
}

fn status(args: StatusArgs) {
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let cfg = Config::load(&cwd);
    let today = today_str();
    let ledger = harness_core::ledger::Ledger::load(&cfg.state_dir);
    let day_usd = ledger.day_total(&today);

    if args.json {
        let pressure = gate::budget_pressure(day_usd, cfg.daily_warn_usd);
        println!(
            "{}",
            serde_json::json!({
                "day_usd": day_usd,
                "daily_warn_usd": cfg.daily_warn_usd,
                "daily_block_usd": cfg.daily_block_usd,
                "pressure": pressure,
            })
        );
        return;
    }

    println!("enabled:            {}", cfg.enabled);
    println!("state_dir:          {}", cfg.state_dir.display());
    println!();
    println!("session.warn_usd:   {:.2}", cfg.session_warn_usd);
    println!("session.block_usd:  {:.2}", cfg.session_block_usd);
    println!("daily.warn_usd:     {:.2}", cfg.daily_warn_usd);
    println!("daily.block_usd:    {:.2}", cfg.daily_block_usd);
    println!();
    println!("today ({today}):     ${day_usd:.4} spent");
}

#[cfg(test)]
mod tests {
    use super::date_key;
    use chrono::{FixedOffset, TimeZone, Utc};

    #[test]
    fn date_key_follows_the_carried_local_offset_not_utc() {
        // 2026-01-01T23:30:00Z is still Jan 1 in UTC, but Jan 2 at +09:00 (JST 08:30).
        // The daily-reset key must follow the LOCAL calendar day, so a +09:00 user's
        // budget rolls over at local midnight — not at 09:00 (UTC midnight).
        let instant = Utc.with_ymd_and_hms(2026, 1, 1, 23, 30, 0).unwrap();
        assert_eq!(date_key(instant), "2026-01-01", "UTC view is Jan 1");

        let jst = FixedOffset::east_opt(9 * 3600).unwrap();
        assert_eq!(
            date_key(instant.with_timezone(&jst)),
            "2026-01-02",
            "at +09:00 the local calendar day is already Jan 2"
        );
    }
}
