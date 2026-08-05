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

    let report = gate::evaluate(&cfg, &root);

    if report.all_green() {
        state::reset(&cfg.state_dir, &session);
        log_event(&cfg, &session, "green", &ran_names(&report), 0);
        if interactive {
            println!("{}", gate::human_report(&report));
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
    let failing: Vec<String> = report.blocking().iter().map(|o| o.name.clone()).collect();

    if attempt > cfg.max_attempts {
        state::reset(&cfg.state_dir, &session);
        log_event(&cfg, &session, "giveup", &failing, attempt);
        // DURABLE SENTINEL (backlog 5151605e part 3). The stop is still allowed
        // here — that is deliberate and unchanged, so a genuinely stuck agent is
        // never trapped. What must not survive is the give-up being INVISIBLE:
        // until now the only trace was this stderr line (gone with the
        // transcript) and a JSONL line in donegate's own private state dir that
        // no tool reads. Downstream — a human, a script, the next session — had
        // no way to tell "the gate enforced and the checks went green" from
        // "the gate stopped enforcing while the checks were still red".
        //
        // The trace goes to the overwatch violation ledger rather than to a new
        // mechanism: donegate already writes every BLOCK there (see the
        // `Outcome::Blocked` call below), overwatch is this repo's project-wide
        // review surface, and the ledger is an append-only file under
        // `~/.overwatch/<project>/` — durable across processes and queryable by
        // both a human and a script.
        //
        // `Outcome::GaveUp` gives it its own signature (`donegate:giveup:<check>`,
        // never `donegate:<check>`), so a give-up can never be read as just
        // another block.
        //
        // WHICH COMMAND SHOWS IT — measured, not assumed (probe of this branch,
        // 2026-08-06: an isolated HOME, four Stops against a `cmd = "exit 1"`
        // check, then the overwatch CLI over that store):
        //
        //   `overwatch violations` → lists it immediately, as its own row:
        //     `donegate:giveup:typecheck  occurrences=1` alongside
        //     `donegate:typecheck  occurrences=3`. This is the query to use.
        //
        //   `overwatch review-queue` → does NOT show a single give-up. That
        //     surface carries only signatures already escalated to SYSTEMIC
        //     (`is_systemic`, default 3 occurrences in 24h), and the probe's
        //     `review-queue --json` was `[]`. It starts carrying a check that
        //     keeps exhausting the cap once recurrence crosses the threshold —
        //     which is a property worth having, but it is not the first-occurrence
        //     channel and must not be described as one.
        emit_violations(&root, &session, &failing, Outcome::GaveUp);
        eprintln!(
            "donegate: {} required check(s) still failing after {} attempts ({}). \
             Allowing stop — fix manually. This give-up is recorded as \
             `donegate:giveup:<check>` in the overwatch violation ledger \
             (`overwatch violations`); the checks are still RED.",
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
    emit_violations(&root, &session, &failing, Outcome::Blocked);
    let reason = gate::block_reason(&report, attempt, cfg.max_attempts);

    if interactive {
        eprintln!("{}", gate::human_report(&report));
        eprintln!("\n{reason}");
        harness_core::hook_latency::record(
            "donegate",
            &session,
            __start.elapsed().as_millis() as u64,
        );
        std::process::exit(1);
    }
    // Stop hook: the JSON `decision` field blocks the stop; the exit code stays 0
    // toward Claude (unchanged protocol). `stop_decision()` is the shared type's
    // Stop-hook channel: `Clean` yields `None`, and both non-clean arms — a
    // violation and an undetermined — yield the same blocking JSON, so there is
    // no arm that could let a non-green gate end the turn.
    let blocked = harness_core::verdict::Verdict::violation(reason);
    match blocked.stop_decision() {
        Some(decision) => println!("{decision}"),
        // Unreachable for a Violation, but resolved to the restricted side rather
        // than to silence (which the Stop protocol reads as "allow").
        None => println!(
            "{}",
            json!({ "decision": "block", "reason": "donegate: required checks failed" })
        ),
    }
    harness_core::hook_latency::record("donegate", &session, __start.elapsed().as_millis() as u64);
    std::process::exit(0);
}

/// Why donegate is recording these checks — the two are NOT the same event and
/// must never share a signature.
///
/// A `Blocked` event says "the gate judged and enforced". A `GaveUp` event says
/// "the gate judged, found the same checks still failing, and stopped
/// enforcing" — the stop is allowed anyway (attempt cap), so this event is the
/// only durable statement that the checks are still red. Collapsing the two
/// would put the give-up back where it started: indistinguishable from a pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Blocked,
    GaveUp,
}

impl Outcome {
    /// The `check_kind` discriminator recorded for check `name`. The give-up
    /// prefix rides inside the discriminator (yielding `donegate:giveup:<name>`
    /// vs `donegate:<name>`) rather than in a separate field, because
    /// `overwatch`'s recurrence detection buckets purely by signature: a
    /// separate field would be invisible to it, and a check that keeps
    /// exhausting the cap would never escalate as its own systemic issue.
    fn check_kind(self, name: &str) -> String {
        match self {
            Outcome::Blocked => name.to_string(),
            Outcome::GaveUp => format!("giveup:{name}"),
        }
    }
}

/// Record one fleet-level violation per failing check, for cross-gate
/// correlated-error detection (`overwatch::violation`). Fail-soft with respect
/// to CONTROL FLOW: never changes the gate's exit code/stdout, never panics if
/// the overwatch store is unwritable (mirrors mutategate's `emit_violation`,
/// crates/mutategate/src/main.rs). `task_key` is set to the session id (rather
/// than a distinct per-attempt id) because a Stop-hook gate has no separate
/// "task" concept below the session/turn it fires in — unlike condukt's
/// per-worker tasks, one donegate invocation IS the unit.
///
/// It is NOT fail-soft with respect to VISIBILITY. A failed append used to be
/// erased by `let _ = …`, which for `Outcome::GaveUp` would silently drop the
/// only durable trace that the gate stopped enforcing — the exact erasure this
/// function is now here to prevent. The write is still best-effort (the stop
/// decision is not this function's to make), but a write that did not land, or
/// an event that could not be built at all, now says so on stderr instead of
/// leaving the caller believing a sentinel exists.
fn emit_violations(root: &std::path::Path, session: &str, failing: &[String], outcome: Outcome) {
    let now = overwatch::store::now();
    for name in failing {
        let kind = outcome.check_kind(name);
        let raw = overwatch::violation::RawViolation {
            check_kind: Some(kind.as_str()),
            ..Default::default()
        };
        let event = overwatch::violation::build_event(
            overwatch::violation::ViolationSource::Donegate,
            &raw,
            session.to_string(),
            session.to_string(),
            now,
            None,
        );
        match event {
            Some(event) => {
                if let Err(e) = overwatch::store::append_violation(root, &event) {
                    eprintln!(
                        "donegate: WARNING could not record the `{}` event for check '{name}' in \
                         the overwatch violation ledger: {e}. This run is NOT represented there.",
                        event.signature
                    );
                }
            }
            // `build_event` returns None only for a blank discriminator, which
            // config loading already filters out — but if it ever happens the
            // event is dropped, and a dropped give-up sentinel must not be silent.
            None => eprintln!(
                "donegate: WARNING check name '{name}' produced no recordable violation \
                 signature ({outcome:?}); nothing was written to the overwatch ledger."
            ),
        }
    }
}

fn ran_names(v: &gate::GateReport) -> Vec<String> {
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

#[cfg(test)]
mod violation_emission_tests {
    use super::{emit_violations, Outcome};
    use crate::config::HOME_ENV_LOCK;

    // `overwatch::store::append_violation`/`scan_violations` resolve their
    // storage root via `harness_core::config::home()`, which reads `$HOME`.
    // Shares `config::HOME_ENV_LOCK` with `config::tests` (not a locally
    // scoped lock) — see that static's doc comment for why a per-module lock
    // is not enough to prevent cross-module races on this same env var.
    #[test]
    fn emit_violations_records_one_event_per_failing_check() {
        let _guard = HOME_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev_home = std::env::var_os("HOME");
        let temp_home = std::env::temp_dir().join(format!(
            "donegate-emit-violations-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&temp_home).expect("create temp HOME");
        std::env::set_var("HOME", &temp_home);

        let root = temp_home.join("project");
        std::fs::create_dir_all(&root).expect("create project root");
        emit_violations(
            &root,
            "session-x",
            &["build".to_string(), "lint".to_string()],
            Outcome::Blocked,
        );

        let events = overwatch::store::scan_violations(&root)
            .events_or_empty()
            .expect("read_violations");

        match prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        let _ = std::fs::remove_dir_all(&temp_home);

        assert_eq!(events.len(), 2, "expected one event per failing check");
        assert!(events
            .iter()
            .all(|e| e.source == overwatch::violation::ViolationSource::Donegate));
        assert!(events.iter().any(|e| e.signature == "donegate:build"));
        assert!(events.iter().any(|e| e.signature == "donegate:lint"));
    }

    /// A give-up must not be recordable as an ordinary block: the two carry
    /// DIFFERENT signatures, so no consumer of the ledger can read "the gate
    /// stopped enforcing" as "the gate blocked once more".
    #[test]
    fn giveup_events_carry_their_own_signature() {
        let _guard = HOME_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev_home = std::env::var_os("HOME");
        let temp_home =
            std::env::temp_dir().join(format!("donegate-emit-giveup-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp_home);
        std::fs::create_dir_all(&temp_home).expect("create temp HOME");
        std::env::set_var("HOME", &temp_home);

        let root = temp_home.join("project");
        std::fs::create_dir_all(&root).expect("create project root");
        emit_violations(&root, "session-x", &["build".to_string()], Outcome::GaveUp);

        let events = overwatch::store::scan_violations(&root)
            .events_or_empty()
            .expect("read_violations");

        match prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        let _ = std::fs::remove_dir_all(&temp_home);

        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].signature, "donegate:giveup:build",
            "a give-up must be distinguishable from the `donegate:build` block signature"
        );
    }

    #[test]
    fn emit_violations_with_no_failures_records_nothing() {
        let _guard = HOME_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev_home = std::env::var_os("HOME");
        let temp_home = std::env::temp_dir().join(format!(
            "donegate-emit-violations-empty-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&temp_home).expect("create temp HOME");
        std::env::set_var("HOME", &temp_home);

        let root = temp_home.join("project");
        std::fs::create_dir_all(&root).expect("create project root");
        emit_violations(&root, "session-x", &[], Outcome::Blocked);

        let events = overwatch::store::scan_violations(&root)
            .events_or_empty()
            .expect("read_violations");

        match prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        let _ = std::fs::remove_dir_all(&temp_home);

        assert!(events.is_empty());
    }
}
