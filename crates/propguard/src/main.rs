//! propguard — a property gate for Claude Code.
//!
//! `tdd` runs concrete tests: it proves specific cases but never formalizes the
//! *semantic* invariants the generated code must satisfy. propguard closes that
//! gap. On **Stop**, it sources the current task's `done_criteria`, deterministically
//! derives 3–5 semantic *properties* from it (idempotence, error-path-returns-Err,
//! output-schema stability, bounds/monotonicity, no-partial-write, determinism —
//! a keyword taxonomy inspired by PGS, arXiv:2506.18315), checks the generated
//! code against them, and BLOCKS when fewer than a configured `threshold` hold.
//!
//! Two modes, like reviewgate:
//!   * `inject` — block once per new diff and inject the property checklist so the
//!     running (subscription) agent self-verifies its own code. No API key.
//!   * `subprocess` — run a configured `checker_cmd` that reports one
//!     `PROP <id>: PASS|FAIL` verdict per property; propguard counts the PASSes.
//!
//! Failure modes are split deliberately (mirroring the sibling gates):
//!   * a *harness* error (bad config, no git, our own bug) → exit 0, allow.
//!   * no done_criteria / nothing checkable → allow (never invent a finding).
//!   * fewer than `threshold` properties satisfied → block (bounded, escapable).
//!   * a *checker* that itself fails → block (bounded) then give up loudly, so a
//!     broken checker can never become a bypass.
//!   * a genuine panic → swallowed to exit 0 by the never-break-a-turn guard.

mod config;
mod derive;
mod gate;
mod git;
mod install;
mod model;
mod state;

use std::path::Path;

use clap::{Parser, Subcommand};
use harness_core::hook::read_stdin;
use serde_json::json;

use config::{Config, Mode};
use gate::Decision;
use model::HookInput;

#[derive(Parser)]
#[command(
    name = "propguard",
    version,
    about = "Property gate for Claude Code: derive semantic properties from done_criteria and block on Stop when too few hold."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Stop hook: derive properties from done_criteria and block until enough hold.
    Check,
    /// Show the properties derived from a given done_criteria string (offline).
    Derive {
        /// The done_criteria text to derive properties from.
        criteria: String,
    },
    /// Merge the propguard Stop hook into ~/.claude/settings.json.
    Install {
        #[arg(long)]
        dry_run: bool,
    },
    /// Remove the propguard Stop hook from ~/.claude/settings.json.
    Uninstall {
        #[arg(long)]
        dry_run: bool,
    },
    /// Write a starter ./propguard.toml.
    Init {
        #[arg(long)]
        force: bool,
    },
    /// Show the resolved config + the properties for the current task/cwd.
    Status,
    /// Trust the current project so its ./propguard.toml (incl. checker_cmd) is honored.
    Trust,
}

fn main() {
    let cli = Cli::parse();
    match cli.command {
        Command::Check => check_command(),
        Command::Derive { criteria } => derive_command(&criteria),
        Command::Install { dry_run } => exit_on_err(install::install(dry_run)),
        Command::Uninstall { dry_run } => exit_on_err(install::uninstall(dry_run)),
        Command::Init { force } => exit_on_err(init(force)),
        Command::Status => status(),
        Command::Trust => exit_on_err(trust()),
    }
}

fn derive_command(criteria: &str) {
    let cfg = Config::default();
    let props = derive::derive_properties(criteria, cfg.min_properties, cfg.max_properties);
    println!("done_criteria: {}", criteria.trim());
    println!("derived {} propert(y|ies):", props.len());
    for (i, p) in props.iter().enumerate() {
        println!("  {}. [{}] {}", i + 1, p.id, p.title);
        println!("     → {}", p.check_hint);
    }
    println!(
        "block threshold (default): {}",
        cfg.threshold.min(props.len()).max(1)
    );
}

fn trust() -> anyhow::Result<()> {
    let root = std::env::current_dir()?;
    let key = harness_core::trust::add(&root)?;
    println!("trusted {}", key.display());
    println!(
        "propguard will now honor {}.",
        Config::project_path(&key).display()
    );
    Ok(())
}

fn exit_on_err(r: anyhow::Result<()>) {
    if let Err(e) = r {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

/// The Stop hook. Always exits 0 toward Claude (the `decision` field, not the
/// exit code, is what blocks a stop). The never-break-a-turn panic guard lives
/// in `harness_core::gate::run`.
fn check_command() -> ! {
    let raw = read_stdin();
    let hook = HookInput::parse(&raw);
    let interactive = hook.is_none();
    harness_core::gate::run::run_guarded("propguard", interactive, move || check_run(hook))
}

fn check_run(hook: Option<HookInput>) -> ! {
    let __start = std::time::Instant::now();
    let interactive = hook.is_none();
    let input = hook.unwrap_or_default();
    let root = input.cwd_or_current();

    if Config::disabled_env() {
        if interactive {
            eprintln!("propguard: disabled (PROPGUARD_DISABLE)");
        }
        harness_core::hook_latency::record("propguard", "", __start.elapsed().as_millis() as u64);
        std::process::exit(0);
    }

    let cfg = Config::load(&root);
    if !cfg.enabled {
        if interactive {
            eprintln!("propguard: disabled in config");
        }
        harness_core::hook_latency::record("propguard", "", __start.elapsed().as_millis() as u64);
        std::process::exit(0);
    }

    let session = input.session_key();

    // one-shot escape hatch
    if let Some(reason) = harness_core::gate::run::consume_skip(&root, ".propguard-skip") {
        state::reset(&cfg.state_dir, &session);
        log_event(&cfg, &session, "skip", &[], &[], 0);
        eprintln!("propguard: .propguard-skip consumed — allowing stop ({reason})");
        harness_core::hook_latency::record(
            "propguard",
            &session,
            __start.elapsed().as_millis() as u64,
        );
        std::process::exit(0);
    }

    let prior = state::load(&cfg.state_dir, &session);
    let decision = gate::evaluate(&cfg, &root, &prior);

    match decision {
        Decision::Allow {
            tag,
            attempts,
            last_hash,
        } => {
            if attempts == 0 && last_hash.is_empty() {
                state::reset(&cfg.state_dir, &session);
            } else {
                state::save(
                    &cfg.state_dir,
                    &session,
                    &state::SessionState {
                        attempts,
                        last_hash,
                        last_ts: chrono::Local::now().timestamp(),
                    },
                );
            }
            log_event(&cfg, &session, tag, &[], &[], attempts);
            if interactive {
                println!("propguard: allow ({tag})");
            }
            harness_core::hook_latency::record(
                "propguard",
                &session,
                __start.elapsed().as_millis() as u64,
            );
            std::process::exit(0);
        }
        Decision::Block {
            reason,
            tag,
            files,
            properties,
            attempts,
            last_hash,
        } => {
            state::save(
                &cfg.state_dir,
                &session,
                &state::SessionState {
                    attempts,
                    last_hash,
                    last_ts: chrono::Local::now().timestamp(),
                },
            );
            log_event(&cfg, &session, tag, &files, &properties, attempts);
            emit_overwatch_violations(&root, &session, &properties);
            if interactive {
                eprintln!("{reason}");
                harness_core::hook_latency::record(
                    "propguard",
                    &session,
                    __start.elapsed().as_millis() as u64,
                );
                std::process::exit(1);
            }
            println!("{}", json!({ "decision": "block", "reason": reason }));
            harness_core::hook_latency::record(
                "propguard",
                &session,
                __start.elapsed().as_millis() as u64,
            );
            std::process::exit(0);
        }
    }
}

/// Record a fleet-level overwatch violation for each failing PROP-* property
/// on a Block decision. **Fail-soft**: this must never change the block
/// reason, exit code, or `state::save` outcome, and must never panic — a
/// broken/unwritable overwatch store is silently ignored (an `unwrap_or`
/// wall keeps errors from propagating; there is no `?`/`unwrap`/`expect` in
/// this path). Called only from the `Decision::Block` arm; `Decision::Allow`
/// emits nothing.
fn emit_overwatch_violations(root: &Path, session: &str, properties: &[&str]) {
    let ts = overwatch::store::now();
    let task_key = format!("propguard:{}", root.display());
    for prop_id in properties {
        let raw = overwatch::violation::RawViolation {
            property_id: Some(prop_id),
            ..Default::default()
        };
        let event = overwatch::violation::build_event(
            overwatch::violation::ViolationSource::Propguard,
            &raw,
            task_key.clone(),
            session.to_string(),
            ts,
            None,
        );
        let _ = overwatch::store::append_violation(root, &event);
    }
}

/// Append one JSONL line per decision. Best effort, local only.
fn log_event(
    cfg: &Config,
    session: &str,
    verdict: &str,
    files: &[String],
    properties: &[&str],
    attempt: u32,
) {
    let entry = json!({
        "ts": chrono::Local::now().to_rfc3339(),
        "session": session,
        "verdict": verdict,
        "mode": cfg.mode.as_str(),
        "files": files,
        "properties": properties,
        "attempt": attempt,
    });
    harness_core::gate::run::append_jsonl(&cfg.state_dir, &entry);
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
    println!("config:         {}", src.display());
    println!("enabled:        {}", cfg.enabled);
    println!("mode:           {}", cfg.mode.as_str());
    if cfg.mode == Mode::Subprocess {
        println!("checker_cmd:    {}", cfg.checker_cmd);
    }
    println!("min_properties: {}", cfg.min_properties);
    println!("max_properties: {}", cfg.max_properties);
    println!("threshold:      {}", cfg.threshold);
    println!("max_attempts:   {}", cfg.max_attempts);
    println!("criteria_file:  {}", cfg.criteria_file);
    println!("state_dir:      {}", cfg.state_dir.display());

    match derive::source_criteria(&cfg, &root) {
        None => println!(
            "done_criteria:  (none found — set PROPGUARD_CRITERIA, write {}, or config done_criteria; every stop is allowed)",
            cfg.criteria_file
        ),
        Some(criteria) => {
            let props = derive::derive_properties(&criteria, cfg.min_properties, cfg.max_properties);
            let threshold = cfg.threshold.min(props.len()).max(1);
            println!("done_criteria:  {}", criteria.trim());
            println!("derived {} propert(y|ies) (block if < {threshold} hold):", props.len());
            for (i, p) in props.iter().enumerate() {
                println!("  {}. [{}] {}", i + 1, p.id, p.title);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// init: write a starter propguard.toml.
// ---------------------------------------------------------------------------

fn init(force: bool) -> anyhow::Result<()> {
    let root = std::env::current_dir()?;
    std::fs::create_dir_all(Config::default().state_dir)?;
    let path = Config::project_path(&root);
    if path.exists() && !force {
        anyhow::bail!(
            "{} already exists (use --force to overwrite)",
            path.display()
        );
    }
    std::fs::write(&path, STARTER)?;
    println!("wrote {}", path.display());
    println!("Review the settings, then run `propguard install` once to wire the Stop hook.");
    Ok(())
}

const STARTER: &str = r#"# propguard.toml — property gate for Claude Code.
#
# On Stop, propguard derives 3–5 semantic properties (invariants) from the current
# task's done_criteria and checks the generated code against them before the agent
# can declare done. It blocks when fewer than `threshold` properties hold.
#
#   mode = "inject"     block once per new diff and inject the property checklist;
#                       the running agent self-verifies its code (no API key).
#   mode = "subprocess" run `checker_cmd` as an independent checker that reports
#                       one `PROP <id>: PASS|FAIL` line per property.
#
# done_criteria is sourced (in priority order) from: the PROPGUARD_CRITERIA env
# var, the `criteria_file` below (condukt / the agent writes it), then the inline
# `done_criteria` value here. With no source, every stop is allowed.

enabled = true
mode = "inject"

min_properties = 3        # floor: pad with baseline invariants if criteria is thin
max_properties = 5        # cap (the task formalizes 3–5 properties)
threshold = 3             # block when fewer than this many properties hold
max_attempts = 2          # consecutive rounds before giving up (allow stop)
reset_after_secs = 600    # per-session counter resets after this idle gap
min_changed_files = 1     # don't check fewer than this many changed files
max_diff_bytes = 200000   # cap the diff handed to the hasher / checker

criteria_file = ".propguard-criteria"

# Only files matching `include` (and not `exclude`) are checked. Omit for the
# built-in source-extension defaults.
# include = ["**/*.rs", "**/*.ts", "**/*.py"]
# exclude = ["**/*.lock", "**/node_modules/**", "**/target/**"]

# Inline fallback done_criteria (lowest priority).
# done_criteria = "冪等に再実行でき、失敗時は panic せずエラーを返し、出力スキーマを壊さないこと"

# subprocess mode only: a command that reads the check prompt on stdin and prints
# `PROP <id>: PASS|FAIL` lines on stdout.
# checker_cmd = "claude -p"
# checker_timeout_secs = 300
"#;
