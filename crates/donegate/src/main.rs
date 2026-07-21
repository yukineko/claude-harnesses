//! donegate — a completion-verification gate for Claude Code.
//!
//! One binary, one subcommand per job. The `gate` subcommand is the **Stop**
//! hook: it runs the project's acceptance commands as subprocesses and, if any
//! required check fails, blocks the stop and feeds the failure back so the agent
//! keeps working. It is the dynamic, "does it actually run?" complement to the
//! static (precommit) and spec-drift gates.
//!
#![deny(clippy::panic)]
//!
//! Failure modes are split deliberately:
//!   * a *harness* error (bad config, no checks, our own bug) → exit 0, allow the
//!     stop. We must never trap a turn because donegate itself broke.
//!   * a *check* failure → block on purpose, with an actionable reason.

mod config;
mod gate;
mod git;
mod install;
mod model;
mod runner;
mod state;

use std::io::Read;
use std::path::Path;

use clap::{Parser, Subcommand};
use serde_json::json;

use config::Config;
use model::HookInput;

#[derive(Parser)]
#[command(
    name = "donegate",
    version,
    about = "Completion-verification gate for Claude Code: run acceptance checks on Stop; block until green."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Stop hook: run applicable checks; block the stop until they pass.
    Gate,
    /// Merge the donegate Stop hook into ~/.claude/settings.json.
    Install {
        #[arg(long)]
        dry_run: bool,
    },
    /// Remove the donegate Stop hook from ~/.claude/settings.json.
    Uninstall {
        #[arg(long)]
        dry_run: bool,
    },
    /// Write a starter ./donegate.toml (auto-detecting the project type).
    Init {
        #[arg(long)]
        force: bool,
    },
    /// Show the resolved config + which checks would run for the cwd.
    Status,
    /// Trust the current project so its ./donegate.toml commands are honored.
    Trust,
}

fn read_stdin() -> String {
    let mut buf = String::new();
    let _ = std::io::stdin().read_to_string(&mut buf);
    buf
}

fn main() {
    let cli = Cli::parse();
    match cli.command {
        Command::Gate => gate_command(),
        Command::Install { dry_run } => exit_on_err(install::install(dry_run)),
        Command::Uninstall { dry_run } => exit_on_err(install::uninstall(dry_run)),
        Command::Init { force } => exit_on_err(init(force)),
        Command::Status => status(),
        Command::Trust => exit_on_err(trust_cmd()),
    }
}

/// Add the current project root to the shared workspace-trust list so its
/// project-local `donegate.toml` commands are honored on Stop.
fn trust_cmd() -> anyhow::Result<()> {
    let root = std::env::current_dir()?;
    let key = harness_core::trust::add(&root)?;
    println!("trusted {}", key.display());
    let proj = Config::project_path(&root);
    if proj.exists() {
        println!(
            "donegate will now run the [[check]] commands in {}",
            proj.display()
        );
    } else {
        println!(
            "(no {} yet — run `donegate init` to create one)",
            proj.display()
        );
    }
    Ok(())
}

fn exit_on_err(r: anyhow::Result<()>) {
    if let Err(e) = r {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

/// The Stop hook. Always exits 0 toward Claude (the `decision` field, not the
/// exit code, is what blocks a stop). Returns exit 1 only in manual CLI mode.
///
/// The never-break-a-turn panic guard lives in `harness_core::gate::run`: a
/// panic in `gate_run` fails CLOSED in hook mode (emits a `decision:block`,
/// bounded to one block via `stop_hook_active` so it can't trap the session) and
/// is surfaced (exit 1) in manual CLI mode. Real `process::exit` calls inside
/// `gate_run` terminate directly, so only genuine panics ever reach the guard.
fn gate_command() -> ! {
    let raw = read_stdin();
    let hook = HookInput::parse(&raw);
    let interactive = hook.is_none();
    // On a post-block re-entry Claude Code sets stop_hook_active; the panic guard
    // uses it to bound a fail-closed block to a single occurrence (no turn-trap).
    let stop_hook_active = hook.as_ref().is_some_and(|h| h.stop_hook_active);
    harness_core::gate::run::run_guarded("donegate", interactive, stop_hook_active, move || {
        gate_run(hook)
    })
}

fn gate_run(hook: Option<HookInput>) -> ! {
    let __start = std::time::Instant::now();
    let interactive = hook.is_none();
    let input = hook.unwrap_or_default();
    let root = input.cwd_or_current();

    if Config::disabled_env() {
        if interactive {
            eprintln!("donegate: disabled (DONEGATE_DISABLE)");
        }
        harness_core::hook_latency::record("donegate", "", __start.elapsed().as_millis() as u64);
        std::process::exit(0);
    }

    let cfg = Config::load(&root);
    if !cfg.enabled || cfg.checks.is_empty() {
        if interactive {
            eprintln!(
                "donegate: nothing to do — {}",
                if !cfg.enabled {
                    "disabled in config".to_string()
                } else {
                    format!(
                        "no [[check]] configured (looked for {})",
                        Config::project_path(&root).display()
                    )
                }
            );
        }
        harness_core::hook_latency::record("donegate", "", __start.elapsed().as_millis() as u64);
        std::process::exit(0);
    }

    let session = input.session_key();

    // one-shot escape hatch
    if let Some(reason) = harness_core::gate::run::consume_skip(&root, ".donegate-skip") {
        state::reset(&cfg.state_dir, &session);
        log_event(&cfg, &session, "skip", &[], 0);
        eprintln!("donegate: .donegate-skip consumed — allowing stop ({reason})");
        harness_core::hook_latency::record(
            "donegate",
            &session,
            __start.elapsed().as_millis() as u64,
        );
        std::process::exit(0);
    }

    let verdict = gate::evaluate(&cfg, &root);

    if verdict.all_green() {
        state::reset(&cfg.state_dir, &session);
        log_event(&cfg, &session, "green", &ran_names(&verdict), 0);
        if interactive {
            println!("{}", gate::human_report(&verdict));
        }
        harness_core::hook_latency::record(
            "donegate",
            &session,
            __start.elapsed().as_millis() as u64,
        );
        std::process::exit(0);
    }

    // blocking failures present
    let attempt = state::bump(&cfg.state_dir, &session, cfg.reset_after_secs);
    let failing: Vec<String> = verdict.blocking().iter().map(|o| o.name.clone()).collect();

    if attempt > cfg.max_attempts {
        state::reset(&cfg.state_dir, &session);
        log_event(&cfg, &session, "giveup", &failing, attempt);
        eprintln!(
            "donegate: {} required check(s) still failing after {} attempts ({}). \
             Allowing stop — fix manually.",
            failing.len(),
            cfg.max_attempts,
            failing.join(", ")
        );
        harness_core::hook_latency::record(
            "donegate",
            &session,
            __start.elapsed().as_millis() as u64,
        );
        std::process::exit(0);
    }

    log_event(&cfg, &session, "blocked", &failing, attempt);
    let reason = gate::block_reason(&verdict, attempt, cfg.max_attempts);

    if interactive {
        eprintln!("{}", gate::human_report(&verdict));
        eprintln!("\n{reason}");
        harness_core::hook_latency::record(
            "donegate",
            &session,
            __start.elapsed().as_millis() as u64,
        );
        std::process::exit(1);
    }
    // Stop hook: JSON decision blocks the stop; exit 0.
    println!("{}", json!({ "decision": "block", "reason": reason }));
    harness_core::hook_latency::record("donegate", &session, __start.elapsed().as_millis() as u64);
    std::process::exit(0);
}

fn ran_names(v: &gate::Verdict) -> Vec<String> {
    v.ran.iter().map(|o| o.name.clone()).collect()
}

/// Append one JSONL line per gate decision. Best effort, local only.
fn log_event(cfg: &Config, session: &str, verdict: &str, names: &[String], attempt: u32) {
    let entry = json!({
        "ts": chrono::Local::now().to_rfc3339(),
        "session": session,
        "verdict": verdict,
        "checks": names,
        "attempt": attempt,
    });
    harness_core::gate::run::append_jsonl(&cfg.state_dir, &entry);
}

fn status() {
    let root = std::env::current_dir().unwrap_or_else(|_| Path::new(".").to_path_buf());
    let cfg = Config::load(&root);
    let proj = Config::project_path(&root);
    let trusted = harness_core::trust::is_trusted(&root);
    let src = if proj.exists() && trusted {
        proj.clone()
    } else if Config::home_path().exists() {
        Config::home_path()
    } else {
        Path::new("(defaults — no config file)").to_path_buf()
    };
    println!("config:        {}", src.display());
    if proj.exists() && !trusted {
        println!(
            "trust:         UNTRUSTED — {} is ignored (run `donegate trust`)",
            proj.display()
        );
    }
    println!("enabled:       {}", cfg.enabled);
    println!("max_attempts:  {}", cfg.max_attempts);
    println!("state_dir:     {}", cfg.state_dir.display());
    println!("checks:        {}", cfg.checks.len());
    if cfg.checks.is_empty() {
        println!("  (none — the gate will allow every stop; run `donegate init`)");
        return;
    }
    match git::changed_files(&root) {
        git::ChangeScan::Files(f) => println!("changed files: {}", f.len()),
        git::ChangeScan::NotRepo => {
            println!("changed files: (not a git repo — all checks unscoped)")
        }
        git::ChangeScan::Failed => {
            println!("changed files: (git state undetermined — all checks unscoped, fail-closed)")
        }
    }
    for c in &cfg.checks {
        let scope = match &c.when_changed {
            Some(g) => format!("when {}", g.join(", ")),
            None => "always".to_string(),
        };
        let opt = if c.optional { " [optional]" } else { "" };
        println!("  - {:<10} {}{}\n      $ {}", c.name, scope, opt, c.cmd);
    }
}

// ---------------------------------------------------------------------------
// init: write a starter donegate.toml, auto-detecting the project type.
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
    let body = starter_config(&root);
    std::fs::write(&path, body)?;
    println!("wrote {}", path.display());
    println!(
        "Review the [[check]] commands, then run `donegate install` once to wire the Stop hook."
    );
    Ok(())
}

const HEADER: &str = r#"# donegate.toml — completion gate for Claude Code.
#
# On Stop, donegate runs every applicable [[check]] as a subprocess. The agent
# can't finish its turn until all REQUIRED checks exit 0. A failing check feeds
# its tail output back so the agent fixes it and continues.
#
# Per check:
#   name          label shown in the block reason
#   cmd           shell command line (sh -c / cmd /C)
#   when_changed  globs (vs git HEAD + untracked); omit to always run
#   timeout_secs  per-check timeout (default below)
#   optional      true = warn on failure but don't block
#   workdir       run in this subdir of the project root
#
# Global:
enabled = true
max_attempts = 3          # give up (allow stop) after N consecutive blocks
default_timeout_secs = 300
output_tail_lines = 40
reset_after_secs = 600    # attempt counter resets after this idle gap
"#;

fn starter_config(root: &Path) -> String {
    let mut s = String::from(HEADER);
    s.push('\n');
    if root.join("Cargo.toml").exists() {
        s.push_str(RUST_CHECKS);
    } else if root.join("package.json").exists() {
        s.push_str(NODE_CHECKS);
    } else if root.join("pyproject.toml").exists()
        || root.join("setup.py").exists()
        || root.join("requirements.txt").exists()
    {
        s.push_str(PY_CHECKS);
    } else if root.join("go.mod").exists() {
        s.push_str(GO_CHECKS);
    } else {
        s.push_str(GENERIC_CHECKS);
    }
    s
}

const RUST_CHECKS: &str = r#"[[check]]
name = "build"
cmd = "cargo build --quiet"
when_changed = ["**/*.rs", "Cargo.toml", "Cargo.lock"]

[[check]]
name = "test"
cmd = "cargo test --quiet"
when_changed = ["**/*.rs"]

[[check]]
name = "clippy"
cmd = "cargo clippy --quiet --all-targets -- -D warnings"
when_changed = ["**/*.rs"]

[[check]]
name = "fmt"
cmd = "cargo fmt --check"
when_changed = ["**/*.rs"]
optional = true
"#;

const NODE_CHECKS: &str = r#"[[check]]
name = "typecheck"
cmd = "npx tsc --noEmit"
when_changed = ["**/*.ts", "**/*.tsx", "tsconfig.json"]

[[check]]
name = "test"
cmd = "npm test --silent"
when_changed = ["**/*.ts", "**/*.tsx", "**/*.js", "**/*.jsx"]

[[check]]
name = "lint"
cmd = "npm run lint --silent"
when_changed = ["**/*.ts", "**/*.tsx", "**/*.js", "**/*.jsx"]
optional = true
"#;

const PY_CHECKS: &str = r#"[[check]]
name = "test"
cmd = "pytest -q"
when_changed = ["**/*.py"]

[[check]]
name = "lint"
cmd = "ruff check ."
when_changed = ["**/*.py"]

[[check]]
name = "types"
cmd = "mypy ."
when_changed = ["**/*.py"]
optional = true
"#;

const GO_CHECKS: &str = r#"[[check]]
name = "build"
cmd = "go build ./..."
when_changed = ["**/*.go", "go.mod", "go.sum"]

[[check]]
name = "test"
cmd = "go test ./..."
when_changed = ["**/*.go"]

[[check]]
name = "vet"
cmd = "go vet ./..."
when_changed = ["**/*.go"]
optional = true
"#;

const GENERIC_CHECKS: &str = r#"# No known build system detected — edit these to match your project.
[[check]]
name = "build"
cmd = "make build"

[[check]]
name = "test"
cmd = "make test"
"#;
