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
//! * an unreadable transcript (missing, a directory) or a Stop payload with an
//!   empty `session_id`/`transcript_path` is an unmeasurable session spend and
//!   blocks/warns per the armed limits;
//! * stdin that is not a hook payload at all blocks the stop
//!   (`{"decision":"block"}`) in every case, limit armed or not, at most 3
//!   consecutive times; the 4th is allowed with a loud `systemMessage`
//!   (`gate_unparseable_input`, `streak`);
//! * no limit configured (no config file, or every USD limit 0) is not "no
//!   limit": the limits are derived from the ledger history or set to the
//!   built-in defaults (`config::Config::load_checked`), and a ledger that
//!   cannot be read for that is a config-undetermined block.
//!
//! Three things still allow, and each is a KNOWN answer rather than a
//! give-up: a readable transcript with nothing to price, `enabled = false` in
//! the config, and `BUDGETGUARD_DISABLE=1` (the operator's explicit escape
//! hatch, checked before the panic guard so it stays reachable).
//!
//! `clippy::panic` (along with `unwrap_used`/`expect_used`) is now enforced via
//! this crate's `[lints] workspace = true` in `Cargo.toml`, which inherits the
//! aggregated `[workspace.lints.clippy]` in the root `Cargo.toml` — this used to
//! be a hand-placed `#![deny(clippy::panic)]` here; it moved to the shared
//! aggregation point instead of being duplicated.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

mod cache;
mod config;
mod gate;
mod install;
mod lock;
mod streak;

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
    /// Budget check for a driver loop (e.g. /flow): one JSON line on stdout;
    /// exit 0 = within budget (or disabled), 1 = over the daily block limit,
    /// 2 = undetermined (config/ledger/cwd unreadable). A caller must treat
    /// every non-zero exit as over budget.
    Check,
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
        Command::Check => check(),
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
    if let Some(h) = &hook {
        // A parsed stop ends any unparseable-stdin streak (see `streak`).
        streak::clear(
            &streak_state_dir(&h.cwd_or_current()),
            session_env().as_deref(),
        );
    }
    harness_core::gate::run::run_guarded("budgetguard", interactive, stop_hook_active, move || {
        gate_run(hook)
    });
    std::process::exit(0);
}

fn gate_run(hook: Option<harness_core::hook::HookInput>) {
    let Some(input) = hook else {
        gate_unparseable_input();
    };
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
            if input.transcript_path.is_empty() || input.session_id.is_empty() {
                // Without a session id and a transcript there is no spend to
                // measure. That is not "within budget": it blocks/warns per
                // the armed limits like any other unmeasured spend (backlog
                // 5a4cd3ff). An enabled config always has a limit armed here:
                // unset limits were derived or defaulted by `load_checked`.
                Some(gate::GateResult {
                    session_usd: None,
                    day_usd: None,
                    cache: None,
                    verdict: gate::undetermined_verdict(
                        &cfg,
                        "the Stop payload carries an empty session_id or transcript_path",
                    ),
                })
            } else {
                let today = today_str();
                gate::evaluate(&cfg, &input.session_id, &input.transcript_path, &today)
            }
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

/// stdin did not parse as a hook payload (a malformed payload, or a manual
/// run). The gate cannot process its input, so it BLOCKS the stop in every
/// case — with or without a limit armed, whatever the config says (user
/// rulings 2026-10-19: 「それは止めるで判断していい。読めないならエラー」 and
/// 「処理できないならエラーでいい」). It is not an exit 1: Claude Code treats a
/// non-zero exit other than 2 as a non-blocking error and lets the stop
/// through. The block is a `{"decision":"block","reason":...}` line on stdout
/// with exit 0, printed directly rather than through the repeat ledger's
/// second-occurrence waiver, because this block has its own bound: at most
/// [`streak::MAX_BLOCKS`] consecutive unparseable stops block, then one is
/// allowed with a loud `systemMessage` (user ruling 「retry3回でぬけだす」; see
/// `streak`). `BUDGETGUARD_DISABLE=1`, checked before stdin is read, bypasses
/// it entirely.
fn gate_unparseable_input() -> ! {
    let session = session_env();
    let state_dir = streak_state_dir(&std::env::current_dir().unwrap_or_default());
    let out = match streak::on_unparseable(&state_dir, session.as_deref()) {
        streak::Action::Block(n) => {
            let reason = format!(
                "budgetguard: stdin is not a Stop hook payload (it could not be parsed as \
                 JSON), so this stop's spend could not be checked at all. The stop is \
                 blocked because the gate cannot process its input; it is not a budget \
                 verdict (unreadable stop {n} of {max}; after {max} in a row the next one \
                 is let through). Set BUDGETGUARD_DISABLE=1 to bypass budgetguard \
                 explicitly.",
                max = streak::MAX_BLOCKS
            );
            eprintln!("{reason}");
            serde_json::json!({ "decision": "block", "reason": reason })
        }
        streak::Action::GiveUp => serde_json::json!({
            "systemMessage": format!(
                "budgetguard: the Stop hook input was unreadable (not JSON) {max} times in a \
                 row; the gate gave up after {max} retries and let this stop through \
                 UNCHECKED. This stop's spend was not checked.",
                max = streak::MAX_BLOCKS
            )
        }),
        streak::Action::MarkerUnusable(why) => serde_json::json!({
            "systemMessage": format!(
                "budgetguard: the Stop hook input was unreadable (not JSON), and the retry \
                 counter that bounds this block could not be used ({why}), so this stop was \
                 let through UNCHECKED. This stop's spend was not checked."
            )
        }),
    };
    println!("{out}");
    std::process::exit(0);
}

/// `CLAUDE_CODE_SESSION_ID`, when set and non-empty: the streak key.
fn session_env() -> Option<String> {
    std::env::var("CLAUDE_CODE_SESSION_ID")
        .ok()
        .filter(|s| !s.trim().is_empty())
}

/// budgetguard's state_dir for the streak marker: the config's when it loads,
/// else the default. The marker only bounds a block, so which of the two it
/// lands in changes no budget verdict.
fn streak_state_dir(cwd: &std::path::Path) -> std::path::PathBuf {
    match Config::load_checked(cwd) {
        Determination::Known(cfg) => cfg.state_dir,
        Determination::Undetermined(_) => config::base_dir().join("state"),
    }
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

/// Print one JSON line with `verdict: "undetermined"` and exit 2. An
/// unmeasured spend must never be reported as headroom, so this carries no
/// thresholds and no `pressure` key.
fn check_undetermined(why: &str) -> ! {
    println!(
        "{}",
        serde_json::json!({ "verdict": "undetermined", "error": why })
    );
    std::process::exit(2);
}

/// `budgetguard check`: the budget question a driver loop asks before each
/// pick. Exit 0 = within (or disabled by the operator), exit 1 = over the
/// daily block limit, exit 2 = undetermined. A corrupt or unreadable ledger
/// is undetermined here (as in `status`), never 0.0 spend.
fn check() {
    if Config::disabled_env() {
        println!("{}", serde_json::json!({ "verdict": "disabled" }));
        return;
    }
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(e) => check_undetermined(&format!("could not resolve the current directory: {e}")),
    };
    let cfg = match Config::load_checked(&cwd) {
        Determination::Known(cfg) => cfg,
        Determination::Undetermined(why) => check_undetermined(why.as_str()),
    };
    if !cfg.enabled {
        println!("{}", serde_json::json!({ "verdict": "disabled" }));
        return;
    }
    let ledger = match harness_core::ledger::Ledger::load_checked(&cfg.state_dir) {
        Ok(ledger) => ledger,
        Err(e) => check_undetermined(&e.to_string()),
    };
    let day_usd = ledger.day_total(&today_str());
    if !day_usd.is_finite() {
        check_undetermined("today's ledger total is not a finite number");
    }
    let over = cfg.daily_block_usd > 0.0 && day_usd >= cfg.daily_block_usd;
    let mut out = serde_json::json!({
        "verdict": if over { "over" } else { "within" },
        "day_usd": day_usd,
        "daily_warn_usd": cfg.daily_warn_usd,
        "daily_block_usd": cfg.daily_block_usd,
        "limits_source": cfg.limits_source.as_str(),
        "pressure": gate::budget_pressure(day_usd, cfg.daily_warn_usd),
    });
    add_session_info(&mut out, &cfg, &ledger);
    println!("{out}");
    if over {
        std::process::exit(1);
    }
}

/// Session figures for `check` / `status --json`, information only (user
/// ruling 2026-10-19 「警告はださなくていい、結果がみたいだけ」): the session
/// Tukey fence when the limits were derived or defaulted, and this session's
/// last recorded spend (`CLAUDE_CODE_SESSION_ID`) when the ledger has one. A
/// key is omitted, never printed as 0, when the figure is not known.
fn add_session_info(
    out: &mut serde_json::Value,
    cfg: &Config,
    ledger: &harness_core::ledger::Ledger,
) {
    if let Some(fence) = cfg.session_fence_usd {
        out["session_fence_usd"] = serde_json::json!(fence);
    }
    if let Some(usd) = session_env().and_then(|s| ledger.session_cost(&s)) {
        out["session_usd"] = serde_json::json!(usd);
    }
}

/// `status` could not determine today's spend: print no `day_usd` and no
/// `pressure` (either would read as a measured value) and exit 2.
fn status_undetermined(args: &StatusArgs, why: &str) -> ! {
    if args.json {
        println!(
            "{}",
            serde_json::json!({ "error": why, "undetermined": true })
        );
    } else {
        eprintln!("budgetguard: today's spend is undetermined: {why}");
    }
    std::process::exit(2);
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
    // `load_checked`, not the lenient `load`: a corrupt or unreadable ledger
    // is an unmeasured day total, and printing it as `day_usd: 0` /
    // `pressure: false` would tell a downstream router there is headroom
    // (backlog c1174e86). Same contract as `check`, exit 2.
    let ledger = match harness_core::ledger::Ledger::load_checked(&cfg.state_dir) {
        Ok(ledger) => ledger,
        Err(e) => status_undetermined(&args, &e.to_string()),
    };
    let day_usd = ledger.day_total(&today);
    if !day_usd.is_finite() {
        status_undetermined(&args, "today's ledger total is not a finite number");
    }

    if args.json {
        let pressure = gate::budget_pressure(day_usd, cfg.daily_warn_usd);
        let mut out = serde_json::json!({
            "day_usd": day_usd,
            "daily_warn_usd": cfg.daily_warn_usd,
            "daily_block_usd": cfg.daily_block_usd,
            "session_warn_usd": cfg.session_warn_usd,
            "session_block_usd": cfg.session_block_usd,
            "limits_source": cfg.limits_source.as_str(),
            "pressure": pressure,
        });
        add_session_info(&mut out, &cfg, &ledger);
        println!("{out}");
        return;
    }

    println!("enabled:            {}", cfg.enabled);
    println!("state_dir:          {}", cfg.state_dir.display());
    println!("limits_source:      {}", cfg.limits_source.as_str());
    if let Some(fence) = cfg.session_fence_usd {
        println!("session fence:      {fence:.2} (information only)");
    }
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
