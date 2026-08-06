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
//!   * a *check* failure → block on purpose, with an actionable reason.
//!   * a *refusal / inability to judge* — the project declares checks in a
//!     `donegate.toml` this root is not trusted to run, or a config file that
//!     cannot be read — → **also blocks** (as `Verdict::Undetermined`), naming
//!     the file and the remedy. Until backlog `3135ebb9` these landed on
//!     `checks: 0` and allowed every stop, i.e. a refusal to judge was emitted
//!     as a clean verdict. See [`config::Declaration`].
//!   * *nothing declared* (no `donegate.toml`, no `~/.donegate/config.toml`) →
//!     exit 0, allow. This is an observation of absence, not a refusal: donegate
//!     is installed once and fires in every project on the machine, so a project
//!     that never opted in is not judged at all.
//!   * *our own bug* (a panic) → the `harness_core::gate::run` barrier, which
//!     fails CLOSED (blocks once, bounded by `stop_hook_active`).

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
///
/// Trusting a repository's MAIN working tree is enough for its linked git
/// worktrees: `harness_core::trust::resolve` inherits trust across that one hop
/// (see its docs for why that is not a loosening).
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

    let (cfg, declaration) = Config::resolve(&root);
    if !cfg.enabled {
        // An explicit opt-out written in a config we were entitled to read.
        if interactive {
            eprintln!("donegate: nothing to do — disabled in config");
        }
        harness_core::hook_latency::record("donegate", "", __start.elapsed().as_millis() as u64);
        std::process::exit(0);
    }

    let session = input.session_key();

    // The declaration is resolved BEFORE `checks.is_empty()`, because those two
    // questions used to share one answer: "I am refusing to run the checks this
    // project declared" rendered as `checks: 0`, i.e. as "allow every stop".
    if declaration.is_refusal() {
        refuse(&cfg, &declaration, &root, &session, interactive, __start);
    }

    if cfg.checks.is_empty() {
        // A real observation: nothing was declared (Absent), or a config we DID
        // read declares no `[[check]]`. donegate is opt-in per project, so this
        // allows — see `Declaration::Absent`'s docs for why that is not the same
        // concession as allowing on a refusal.
        if interactive {
            eprintln!(
                "donegate: nothing to do — {}",
                declaration_note(&root, &declaration)
            );
        }
        harness_core::hook_latency::record("donegate", "", __start.elapsed().as_millis() as u64);
        std::process::exit(0);
    }

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
    emit_violations(&root, &session, &failing);
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

/// One line for the `nothing to do` notice, naming WHICH determined answer we
/// got. The two refusal arms are unreachable here (the caller resolved them
/// first) but are still worded as refusals rather than as "nothing to do", so a
/// future reordering cannot make a refusal read as an absence.
fn declaration_note(root: &Path, d: &config::Declaration) -> String {
    match d {
        config::Declaration::Absent => format!(
            "no config declared (looked for {} and {})",
            Config::project_path(root).display(),
            Config::home_path().display()
        ),
        config::Declaration::Loaded(p) => format!("{} declares no [[check]]", p.display()),
        config::Declaration::RefusedUntrusted { project_path } => format!(
            "REFUSING to run the checks declared in {} (project not trusted)",
            project_path.display()
        ),
        config::Declaration::Unreadable { path, why } => {
            format!(
                "REFUSING to judge: {} could not be read ({why})",
                path.display()
            )
        }
    }
}

/// The model-facing text for a refusal. Names the file and the remedy, because a
/// block the operator cannot act on is just a trap.
fn refusal_reason(d: &config::Declaration, attempt: u32, max: u32) -> String {
    let head = match d {
        config::Declaration::RefusedUntrusted { project_path } => format!(
            "🚦 donegate: REFUSING TO JUDGE — this project is not trusted (attempt {attempt}/{max}).\n\n\
             {} declares acceptance checks, but this project root is not on the workspace-trust \
             list, so donegate will not run them. That is a refusal to judge, NOT a pass: donegate \
             cannot tell you whether this project's own checks are green.\n\n\
             Remedy — run once, in this project root:\n    donegate trust\n\n\
             If this root is a git worktree, trusting its MAIN working tree is enough; worktrees \
             inherit that trust.",
            project_path.display()
        ),
        config::Declaration::Unreadable { path, why } => format!(
            "🚦 donegate: REFUSING TO JUDGE — {} could not be read (attempt {attempt}/{max}).\n\
             \n    {why}\n\n\
             A config donegate cannot read is not a config that declares zero checks — it may \
             declare ten required ones. Fix (or remove) the file, then finish.",
            path.display()
        ),
        // Unreachable: only `is_refusal()` answers reach here. Resolved to the
        // restricted side rather than to a reassuring string.
        config::Declaration::Absent | config::Declaration::Loaded(_) => format!(
            "🚦 donegate: REFUSING TO JUDGE — internal error: a non-refusal declaration reached \
             the refusal path (attempt {attempt}/{max}). Treated as undetermined."
        ),
    };
    format!(
        "{head}\n\nOther ways out: DONEGATE_DISABLE=1 (turn donegate off), HARNESS_TRUST_ALL=1 \
         (trust every project), or a `.donegate-skip` file in the project root with a one-line \
         reason (consumed once)."
    )
}

/// The refusal path: donegate could not judge, so it must not let the stop
/// through. Blocks via the shared tri-state's `Undetermined` arm — which is
/// exactly what this is, and which records one telemetry event per give-up so
/// the fleet can see how often the gate is blocking on ignorance.
///
/// **Bounded, and this is stated rather than hidden.** The block is routed
/// through the gate's pre-existing `max_attempts` counter, so after N stops in
/// one session donegate gives up loudly and allows. An agent cannot fix a trust
/// refusal by working harder (it needs an operator to run `donegate trust`), and
/// an unbounded block on a Stop hook traps the session with no way out. That is
/// the same bounded concession `max_attempts` already makes for genuinely
/// failing checks, reused deliberately rather than invented here — it is a
/// bounded fail-open, it is logged (`giveup`) and printed every time, and it is
/// the residual risk of this design.
fn refuse(
    cfg: &Config,
    declaration: &config::Declaration,
    root: &Path,
    session: &str,
    interactive: bool,
    start: std::time::Instant,
) -> ! {
    // The one-shot escape hatch still applies to a refusal.
    if let Some(reason) = harness_core::gate::run::consume_skip(root, ".donegate-skip") {
        state::reset(&cfg.state_dir, session);
        log_event(cfg, session, "skip", &[], 0);
        eprintln!("donegate: .donegate-skip consumed — allowing stop ({reason})");
        harness_core::hook_latency::record("donegate", session, start.elapsed().as_millis() as u64);
        std::process::exit(0);
    }

    let attempt = state::bump(&cfg.state_dir, session, cfg.reset_after_secs);
    if attempt > cfg.max_attempts {
        state::reset(&cfg.state_dir, session);
        log_event(cfg, session, "giveup-refusal", &[], attempt);
        eprintln!(
            "donegate: still unable to judge after {} attempts — {}. Allowing stop; NOTHING WAS \
             VERIFIED.",
            cfg.max_attempts,
            declaration_note(root, declaration)
        );
        harness_core::hook_latency::record("donegate", session, start.elapsed().as_millis() as u64);
        std::process::exit(0);
    }

    log_event(cfg, session, "refused", &[], attempt);
    let reason = refusal_reason(declaration, attempt, cfg.max_attempts);
    // `undetermined`, not `violation`: donegate is not asserting the project is
    // broken, it is asserting it could not tell.
    let verdict = harness_core::verdict::Verdict::undetermined(reason.clone());

    if interactive {
        eprintln!("{reason}");
        harness_core::hook_latency::record("donegate", session, start.elapsed().as_millis() as u64);
        std::process::exit(1);
    }
    match verdict.stop_decision() {
        Some(decision) => println!("{decision}"),
        // Unreachable for an Undetermined; still resolved to the restricted side
        // rather than to silence, which the Stop protocol reads as "allow".
        None => println!(
            "{}",
            json!({ "decision": "block", "reason": "donegate: could not judge this project" })
        ),
    }
    harness_core::hook_latency::record("donegate", session, start.elapsed().as_millis() as u64);
    std::process::exit(0);
}

/// Record one fleet-level violation per failing check, for cross-gate
/// correlated-error detection (`overwatch::violation`). Fail-soft: never
/// changes the gate's exit code/stdout, never panics if the overwatch store
/// is unwritable (mirrors mutategate's `emit_violation`,
/// crates/mutategate/src/main.rs). `task_key` is set to the session id
/// (rather than a distinct per-attempt id) because a Stop-hook gate has no
/// separate "task" concept below the session/turn it fires in — unlike
/// condukt's per-worker tasks, one donegate invocation IS the unit.
fn emit_violations(root: &std::path::Path, session: &str, failing: &[String]) {
    let now = overwatch::store::now();
    for name in failing {
        let raw = overwatch::violation::RawViolation {
            check_kind: Some(name.as_str()),
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
        if let Some(event) = event {
            let _ = overwatch::store::append_violation(root, &event);
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

/// The operator's view. It must state what the gate is going to DO, and the four
/// declaration answers do not share one action — printing `checks: 0` under a
/// refusal (as this did before backlog 3135ebb9) told the operator the gate would
/// allow every stop while it was in fact refusing to run a declared check set.
fn status() {
    let root = std::env::current_dir().unwrap_or_else(|_| Path::new(".").to_path_buf());
    let (cfg, declaration) = Config::resolve(&root);
    match &declaration {
        config::Declaration::Loaded(p) => println!("config:        {}", p.display()),
        config::Declaration::Absent => println!("config:        (defaults — no config file)"),
        config::Declaration::RefusedUntrusted { project_path } => {
            println!(
                "config:        (defaults — {} is REFUSED)",
                project_path.display()
            )
        }
        config::Declaration::Unreadable { path, .. } => {
            println!(
                "config:        (defaults — {} is UNREADABLE)",
                path.display()
            )
        }
    }
    match harness_core::trust::resolve(&root) {
        harness_core::trust::Trust::Direct => println!("trust:         trusted (explicit)"),
        harness_core::trust::Trust::InheritedFromMainWorktree(main) => println!(
            "trust:         trusted (inherited — this is a worktree of {})",
            main.display()
        ),
        harness_core::trust::Trust::Untrusted => println!("trust:         UNTRUSTED"),
    }
    println!("enabled:       {}", cfg.enabled);
    println!("max_attempts:  {}", cfg.max_attempts);
    println!("state_dir:     {}", cfg.state_dir.display());

    if declaration.is_refusal() {
        println!("checks:        (unknown — donegate is refusing to judge this project)");
        println!(
            "verdict:       BLOCK — {}",
            declaration_note(&root, &declaration)
        );
        match &declaration {
            config::Declaration::RefusedUntrusted { .. } => println!(
                "  remedy:      run `donegate trust` here (or trust the main working tree if this \
                 is a worktree)"
            ),
            config::Declaration::Unreadable { why, .. } => println!("  parse error: {why}"),
            _ => {}
        }
        println!(
            "  bound:       after {} consecutive blocks in one session donegate gives up and \
             allows the stop, having verified NOTHING",
            cfg.max_attempts
        );
        return;
    }

    println!("checks:        {}", cfg.checks.len());
    if cfg.checks.is_empty() {
        println!("  (none declared — the gate will allow every stop; run `donegate init`)");
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
    use super::emit_violations;
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
        emit_violations(&root, "session-x", &[]);

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
