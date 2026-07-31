//! budgetguard — real-time cost budget gate for Claude Code.
//!
//! On every Stop it reads the session transcript, computes the USD cost using
//! the same pricing table as gauge, and blocks the turn if session or daily
//! limits are exceeded.
//!
//! "Cannot determine" is NOT "within budget". Where this gate cannot measure
//! what it is asked to measure, it resolves restrictively rather than exiting
//! 0, because an unmeasured spend read as headroom is indistinguishable from a
//! passing check:
//!
//! * a panic in `gate_run` blocks (`gate::run::run_guarded`, see `gate_command`),
//!   bounded by `stop_hook_active` so a deterministic crash cannot trap a turn;
//! * a config file that exists but cannot be read or parsed blocks
//!   (`gate::config_undetermined_result`);
//! * an unmeasurable session spend blocks/warns per the armed limits
//!   (`gate::undetermined_verdict`);
//! * a day total that could not be serialized against other sessions
//!   blocks/warns per the armed DAILY limits (`gate::day_undetermined_verdict`).
//!
//! Three things still exit 0, and each is a KNOWN answer rather than a
//! give-up: no config file at all (the operator configured nothing), no
//! transcript data to price, and `BUDGETGUARD_DISABLE=1` (the operator's
//! explicit escape hatch, checked before the panic guard so it stays reachable).
#![deny(clippy::panic)]

mod cache;
mod config;
mod gate;
mod install;
mod lock;

use clap::{Args, Parser, Subcommand};

use harness_core::hook::read_stdin;
use harness_core::verdict::Determination;

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
    let result = match Config::load_checked(&cwd) {
        Determination::Known(cfg) => {
            if !cfg.enabled {
                return;
            }
            let today = today_str();
            gate::evaluate(&cfg, &input.session_id, &input.transcript_path, &today)
        }
        // A config file exists but could not be read or parsed. Which limits the
        // operator armed is exactly what is unknown, so there is no headroom to
        // report. Bounded: the `stop_hook_active` guard above returns before
        // reaching here on re-entry, so this blocks at most once and cannot trap
        // the turn, and `BUDGETGUARD_DISABLE` is checked outside the panic guard.
        Determination::Undetermined(why) => Some(gate::config_undetermined_result(why.as_str())),
    };
    if let Some(check_kind) = gate::block_check_kind(&result) {
        emit_violation(&cwd, &input.session_id, check_kind);
    }
    gate::emit_and_exit(result);
}

/// Record a fleet-level violation for a blocking Stop, for cross-gate
/// correlated-error detection (`overwatch::violation`). Fail-soft: never
/// changes the gate's exit code/stdout, never panics if the overwatch store
/// is unwritable (mirrors donegate/reviewgate/tdd's `emit_violation[s]`).
/// `task_key` is set to the session id (see those functions' doc comments for
/// why: a Stop-hook gate has no separate "task" concept below the
/// session/turn it fires in).
fn emit_violation(root: &std::path::Path, session: &str, check_kind: &str) {
    let raw = overwatch::violation::RawViolation {
        check_kind: Some(check_kind),
        ..Default::default()
    };
    let event = overwatch::violation::build_event(
        overwatch::violation::ViolationSource::Budgetguard,
        &raw,
        session.to_string(),
        session.to_string(),
        overwatch::store::now(),
        None,
    );
    if let Some(event) = event {
        let _ = overwatch::store::append_violation(root, &event);
    }
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
    // Falling back to "." on an unreadable cwd would silently report a
    // DIFFERENT project's config as if it were this one's.
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(e) => {
            eprintln!("budgetguard: could not resolve the current directory: {e}");
            std::process::exit(2);
        }
    };
    let cfg = match Config::load_checked(&cwd) {
        Determination::Known(cfg) => cfg,
        Determination::Undetermined(why) => {
            // Print no thresholds and no `pressure` key: 0.00 would read as
            // "no limit configured" and `pressure:false` as "there is
            // headroom", when the truth is that neither could be determined.
            if args.json {
                println!(
                    "{}",
                    serde_json::json!({ "error": why.as_str(), "undetermined": true })
                );
            } else {
                eprintln!("budgetguard: {}", why.as_str());
            }
            std::process::exit(2);
        }
    };
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
    println!(
        "cache.min_rate:     {:.2} (effective {:.2})",
        cfg.cache_hit_min_rate,
        cache::effective_threshold(cfg.cache_hit_min_rate)
    );
    println!("cache.min_tokens:   {}", cfg.cache_hit_min_tokens);
    println!();
    println!("today ({today}):     ${day_usd:.4} spent");
}

/// Serializes every test in this crate that mutates the process-global
/// `$HOME` env var (currently only `violation_emission_tests`, which points
/// `$HOME` at a scratch dir to isolate `overwatch::store`'s home-relative
/// storage root). `cargo test` runs a crate's tests on multiple threads by
/// default; an unsynchronized `$HOME` mutation races against any other test
/// doing the same (the same race already found and fixed this way in
/// donegate/reviewgate/tdd's `config.rs`).
#[cfg(test)]
pub(crate) static HOME_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

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

#[cfg(test)]
mod violation_emission_tests {
    use super::*;

    fn with_scratch_home<T>(f: impl FnOnce(&std::path::Path) -> T) -> T {
        let _guard = HOME_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", home.path());
        let root = tempfile::tempdir().unwrap();
        let result = f(root.path());
        std::env::remove_var("HOME");
        result
    }

    #[test]
    fn emit_violation_records_a_budgetguard_event() {
        with_scratch_home(|root| {
            emit_violation(root, "sess-1", "session-budget-exceeded");
            let events = overwatch::store::scan_violations(root)
                .events_or_empty()
                .expect("read_violations");
            assert_eq!(events.len(), 1, "expected exactly one recorded violation");
            assert_eq!(
                events[0].source,
                overwatch::violation::ViolationSource::Budgetguard
            );
            assert_eq!(events[0].signature, "budgetguard:session-budget-exceeded");
        });
    }

    #[test]
    fn emit_violation_with_blank_check_kind_records_nothing() {
        with_scratch_home(|root| {
            emit_violation(root, "sess-1", "   ");
            let events = overwatch::store::scan_violations(root)
                .events_or_empty()
                .expect("read_violations");
            assert!(
                events.is_empty(),
                "a blank discriminator must not build a signature"
            );
        });
    }

    #[test]
    fn block_check_kind_matches_only_block_verdicts() {
        use crate::gate::GateResult;

        assert_eq!(
            gate::block_check_kind(&None),
            None,
            "no result must not be read as a block"
        );
        assert_eq!(
            gate::block_check_kind(&Some(GateResult {
                session_usd: Some(0.0),
                day_usd: Some(0.0),
                verdict: gate::Verdict::Allow,
                cache: None,
            })),
            None
        );
        assert_eq!(
            gate::block_check_kind(&Some(GateResult {
                session_usd: Some(1.0),
                day_usd: Some(1.0),
                verdict: gate::Verdict::Block("over budget".to_string(), "session-budget-exceeded"),
                cache: None,
            })),
            Some("session-budget-exceeded")
        );
    }
}
