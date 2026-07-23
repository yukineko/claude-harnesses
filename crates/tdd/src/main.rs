//! tdd — a test-first gate for Claude Code.
//!
//! `gate` is the **Stop** hook: it blocks the stop when implementation code was
//! added with no accompanying test (you must write tests), and feeds the reason
//! back so the agent writes one and continues. `red` / `green` / `verify` make
//! test-first a *verifiable* artifact for the `/tdd` skill.
//!
//! Failure modes are split deliberately:
//!   * a *harness* error (our own bug, unreadable git) → exit 0, allow the stop.
//!     tdd must never trap a turn because it broke.
//!   * a *missing test* → block on purpose, with an actionable reason.

mod config;
mod gate;
mod git;
mod install;
mod model;
mod proof;
mod runner;
mod state;
mod transition;

use std::io::Read;
use std::path::Path;

use clap::{Parser, Subcommand};
use serde_json::json;

use config::Config;
use model::HookInput;

#[derive(Parser)]
#[command(
    name = "tdd",
    version,
    about = "Test-first gate for Claude Code: block stops when code lands without a test; make RED→GREEN verifiable."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Stop hook: block the stop when implementation changed without a test.
    Gate,
    /// Run the tests and require them to FAIL; record the RED proof (test-first).
    Red {
        #[arg(long)]
        task: String,
        /// Test command (defaults to config `test_cmd`).
        #[arg(long)]
        cmd: Option<String>,
        /// Identity (agent/session id) authoring the test. Recorded into the RED
        /// proof; compared against `tdd green --author` under `strict_separation`.
        #[arg(long)]
        author: Option<String>,
    },
    /// Require a RED proof, run the tests, require them to PASS; record GREEN.
    Green {
        #[arg(long)]
        task: String,
        #[arg(long)]
        cmd: Option<String>,
        /// Identity (agent/session id) authoring the implementation. Under
        /// `strict_separation` this must differ from the RED proof's `--author`,
        /// else `tdd green` is rejected (fail-closed).
        #[arg(long)]
        author: Option<String>,
        /// Explicitly opt **out** of strict test/impl author separation for this
        /// run — highest priority, overriding both the config file and the
        /// gate-crate-context default-on. Use when a gate-crate `tdd green`
        /// legitimately runs with a single identity.
        #[arg(long, conflicts_with = "strict_separation")]
        no_strict_separation: bool,
        /// Explicitly opt **in** to strict test/impl author separation for this
        /// run, regardless of context. Highest priority alongside
        /// `--no-strict-separation` (mutually exclusive).
        #[arg(long)]
        strict_separation: bool,
    },
    /// Exit 0 iff both RED and GREEN proofs exist for the task.
    Verify {
        #[arg(long)]
        task: String,
    },
    /// Classify the RED→GREEN transition for a task and print an oracle report as
    /// JSON. Exit 0 only for a valid Fail→Pass oracle, else exit 1 (fail-soft:
    /// missing/corrupt proofs report `unknown`, never panic).
    Oracle {
        #[arg(long)]
        task: String,
    },
    /// Write a starter ./tdd.toml.
    Init {
        #[arg(long)]
        force: bool,
    },
    /// Merge the tdd Stop hook into ~/.claude/settings.json.
    Install {
        #[arg(long)]
        dry_run: bool,
    },
    /// Remove the tdd Stop hook from ~/.claude/settings.json.
    Uninstall {
        #[arg(long)]
        dry_run: bool,
    },
    /// Trust this project so its `tdd.toml` `test_cmd` is honored (it is executed
    /// verbatim by `tdd red`/`tdd green`; untrusted by default).
    Trust,
    /// Show the resolved config + what the gate would do for the cwd.
    Status,
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
        Command::Red { task, cmd, author } => proof_command(&task, &cmd, &author, true, None),
        Command::Green {
            task,
            cmd,
            author,
            no_strict_separation,
            strict_separation,
        } => {
            // Tri-state CLI override for strict_separation: an explicit flag
            // (either direction) is `Some(_)` and wins over config/gate-context;
            // neither flag → `None` (defer to config file / gate-crate default).
            // The two flags are mutually exclusive (clap `conflicts_with`).
            let strict_override = if no_strict_separation {
                Some(false)
            } else if strict_separation {
                Some(true)
            } else {
                None
            };
            proof_command(&task, &cmd, &author, false, strict_override)
        }
        Command::Verify { task } => verify_command(&task),
        Command::Oracle { task } => oracle_command(&task),
        Command::Init { force } => exit_on_err(init(force)),
        Command::Install { dry_run } => exit_on_err(install::install(dry_run)),
        Command::Uninstall { dry_run } => exit_on_err(install::uninstall(dry_run)),
        Command::Trust => exit_on_err(trust_project()),
        Command::Status => status(),
    }
}

fn exit_on_err(r: anyhow::Result<()>) {
    if let Err(e) = r {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

/// The Stop hook. Always exits 0 toward Claude (the `decision` field, not the
/// exit code, blocks a stop). Returns exit 1 only in manual CLI mode.
///
/// The never-break-a-turn panic guard lives in `harness_core::gate::run`: a
/// panic in `gate_run` fails CLOSED in hook mode (emits a `decision:block`,
/// bounded to one block via `stop_hook_active`) and is surfaced (exit 1) in
/// manual CLI mode. Real `process::exit` calls inside `gate_run` terminate
/// directly, so only genuine panics ever reach the guard.
fn gate_command() -> ! {
    let raw = read_stdin();
    let hook = HookInput::parse(&raw);
    let interactive = hook.is_none();
    // On a post-block re-entry Claude Code sets stop_hook_active; the panic guard
    // uses it to bound a fail-closed block to a single occurrence (no turn-trap).
    let stop_hook_active = hook.as_ref().is_some_and(|h| h.stop_hook_active);
    harness_core::gate::run::run_guarded("tdd", interactive, stop_hook_active, move || {
        gate_run(hook)
    })
}

fn gate_run(hook: Option<HookInput>) -> ! {
    let interactive = hook.is_none();
    let input = hook.unwrap_or_default();
    let root = input.cwd_or_current();

    if Config::disabled_env() {
        if interactive {
            eprintln!("tdd: disabled (TDD_DISABLE)");
        }
        std::process::exit(0);
    }

    let cfg = Config::load(&root);
    if !cfg.enabled {
        if interactive {
            eprintln!("tdd: disabled in config");
        }
        std::process::exit(0);
    }

    let session = input.session_key();

    if let Some(reason) = harness_core::gate::run::consume_skip(&root, ".tdd-skip") {
        state::reset(&cfg.state_dir, &session);
        log_event(&cfg, &session, "skip", 0);
        eprintln!("tdd: .tdd-skip consumed — allowing stop ({reason})");
        std::process::exit(0);
    }

    let verdict = gate::evaluate(&cfg, &root);

    if !verdict.blocks(&cfg) {
        state::reset(&cfg.state_dir, &session);
        log_event(&cfg, &session, "ok", 0);
        if interactive {
            println!("{}", gate::human_report(&verdict, &cfg));
        }
        std::process::exit(0);
    }

    // a blocking finding: no test for new implementation
    let attempt = state::bump(&cfg.state_dir, &session, cfg.reset_after_secs);

    if attempt > cfg.max_attempts {
        state::reset(&cfg.state_dir, &session);
        log_event(&cfg, &session, "giveup", attempt);
        eprintln!(
            "tdd: still no test after {} attempts — allowing stop. Add one or set TDD_DISABLE=1.",
            cfg.max_attempts
        );
        std::process::exit(0);
    }

    log_event(&cfg, &session, "blocked", attempt);
    emit_violation(&root, &session, gate::check_kind(&verdict));
    let reason = gate::block_reason(&verdict, attempt, cfg.max_attempts);

    if interactive {
        eprintln!("{}", gate::human_report(&verdict, &cfg));
        eprintln!("\n{reason}");
        std::process::exit(1);
    }
    println!("{}", json!({ "decision": "block", "reason": reason }));
    std::process::exit(0);
}

/// `tdd red` / `tdd green`. Manual CLI: non-zero exit signals the skill that the
/// phase precondition (must-fail / must-pass, or — under `strict_separation` —
/// distinct test/impl authorship) was not met.
fn proof_command(
    task: &str,
    cmd: &Option<String>,
    author: &Option<String>,
    is_red: bool,
    strict_override: Option<bool>,
) -> ! {
    let root = std::env::current_dir().unwrap_or_else(|_| Path::new(".").to_path_buf());
    let mut cfg = Config::load(&root);
    // Resolve the effective strict_separation before `green` consumes it:
    // explicit CLI/config > gate-crate-context default-on > global default-off.
    // (`red` records the RED author but never checks separation, so this only
    // affects `green`; resolving unconditionally keeps `cfg` consistent.)
    cfg.strict_separation = cfg.resolve_strict_separation(&root, strict_override);
    let r = if is_red {
        proof::red(&root, &cfg, task, cmd, author)
    } else {
        proof::green(&root, &cfg, task, cmd, author)
    };
    match r {
        Ok(()) => std::process::exit(0),
        Err(e) => {
            eprintln!("tdd: {e}");
            std::process::exit(1);
        }
    }
}

fn verify_command(task: &str) -> ! {
    let root = std::env::current_dir().unwrap_or_else(|_| Path::new(".").to_path_buf());
    let cfg = Config::load(&root);
    if proof::verify(&root, &cfg, task) {
        println!("✓ tdd: `{task}` has both RED and GREEN proofs (test-first verified)");
        std::process::exit(0);
    }
    eprintln!(
        "✗ tdd: `{task}` is missing a RED or GREEN proof. Run `tdd red --task {task}` then \
         `tdd green --task {task}`."
    );
    std::process::exit(1);
}

/// `tdd oracle`: classify the task's RED→GREEN transition and print a JSON
/// report. Fail-soft — a missing/unreadable/corrupt proof yields
/// `has_red/has_green=false`, `transition="unknown"`, `valid_fp_oracle=false`
/// and still prints valid JSON. Exit 0 only for a valid Fail→Pass oracle.
fn oracle_command(task: &str) -> ! {
    let root = std::env::current_dir().unwrap_or_else(|_| Path::new(".").to_path_buf());
    let cfg = Config::load(&root);
    let pre = proof::read_passed(&root, &cfg, task, "red");
    let post = proof::read_passed(&root, &cfg, task, "green");
    let report = transition::oracle_report(pre, post);
    let valid = report
        .get("valid_fp_oracle")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    println!("{report}");
    if valid {
        std::process::exit(0);
    }
    std::process::exit(1);
}

fn log_event(cfg: &Config, session: &str, verdict: &str, attempt: u32) {
    let entry = json!({
        "ts": chrono::Local::now().to_rfc3339(),
        "session": session,
        "verdict": verdict,
        "attempt": attempt,
    });
    harness_core::gate::run::append_jsonl(&cfg.state_dir, &entry);
}

/// Record a fleet-level violation for a blocking Stop, for cross-gate
/// correlated-error detection (`overwatch::violation`). Fail-soft: never
/// changes the gate's exit code/stdout, never panics if the overwatch store
/// is unwritable (mirrors donegate's `emit_violations` / reviewgate's
/// `emit_violation`). `task_key` is set to the session id (see those
/// functions' doc comments for why: a Stop-hook gate has no separate "task"
/// concept below the session/turn it fires in).
fn emit_violation(root: &Path, session: &str, check_kind: &str) {
    let raw = overwatch::violation::RawViolation {
        check_kind: Some(check_kind),
        ..Default::default()
    };
    let event = overwatch::violation::build_event(
        overwatch::violation::ViolationSource::Tdd,
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

fn status() {
    let root = std::env::current_dir().unwrap_or_else(|_| Path::new(".").to_path_buf());
    let mut cfg = Config::load(&root);
    // Reflect the same gate-crate-context resolution `tdd green` would apply.
    cfg.strict_separation = cfg.resolve_strict_separation(&root, None);
    let src = if Config::project_path(&root).exists() {
        Config::project_path(&root)
    } else if Config::home_path().exists() {
        Config::home_path()
    } else {
        Path::new("(defaults — no config file)").to_path_buf()
    };
    println!("config:        {}", src.display());
    println!("enabled:       {}", cfg.enabled);
    println!("max_attempts:  {}", cfg.max_attempts);
    println!("min impl lines:{}", cfg.min_added_impl_lines);
    println!("test_cmd:      {}", cfg.test_cmd);
    println!("proof_dir:     {}", cfg.proof_dir);
    println!("state_dir:     {}", cfg.state_dir.display());
    println!("strict_separation: {}", cfg.strict_separation);
    println!();
    let verdict = gate::evaluate(&cfg, &root);
    println!("{}", gate::human_report(&verdict, &cfg));
}

/// `tdd trust`: add the current project root to the shared workspace-trust list
/// so its `tdd.toml` `test_cmd` is honored by `tdd red`/`tdd green`.
fn trust_project() -> anyhow::Result<()> {
    let root = std::env::current_dir()?;
    let key = harness_core::trust::add(&root)?;
    println!("✓ trusted {}", key.display());
    let p = Config::project_path(&root);
    if p.exists() {
        println!("tdd will now honor test_cmd from {}", p.display());
    } else {
        println!("(no {} yet — create one with `tdd init`)", p.display());
    }
    Ok(())
}

fn init(force: bool) -> anyhow::Result<()> {
    let root = std::env::current_dir()?;
    let path = Config::project_path(&root);
    if path.exists() && !force {
        anyhow::bail!(
            "{} already exists (use --force to overwrite)",
            path.display()
        );
    }
    std::fs::write(&path, STARTER_CONFIG)?;
    println!("wrote {}", path.display());
    println!("Then `tdd install` once to wire the Stop hook (or install the plugin).");
    Ok(())
}

const STARTER_CONFIG: &str = r#"# tdd.toml — test-first gate for Claude Code.
#
# On Stop, `tdd gate` blocks the turn when implementation code was ADDED without
# an accompanying test (an inline #[test]/def test_/func Test/it(...), or a file
# under tests/). The block is fed back so the agent writes the test and finishes.
#
# `tdd red`/`tdd green` (driven by the /tdd skill) make test-first verifiable:
# red REQUIRES the tests to fail first; green REQUIRES a prior red then a pass.

enabled = true
max_attempts = 3            # give up (allow stop) after N consecutive blocks
reset_after_secs = 600      # attempt counter resets after this idle gap
min_added_impl_lines = 1    # need a test once this many impl lines are added

# Default command for `tdd red` / `tdd green` (override per call with --cmd).
test_cmd = "cargo test"

# Where RED/GREEN proof artifacts are written (relative to the project root).
proof_dir = ".tdd"

# Files that count as implementation / as tests. Defaults cover Rust, Python,
# TS/JS, Go, Java, Ruby, C/C++, Kotlin, Swift — override to taste.
# impl_globs = ["**/*.rs", "**/*.py", "**/*.ts"]
# test_path_globs = ["**/tests/**", "**/test_*.py", "**/*.spec.*"]
# test_markers = ['#\[\s*test', '\bdef\s+test_', '\bfunc\s+Test\w']

# Opt-in: `tdd green` fail-closed rejects if the RED (test-authoring) identity
# equals the GREEN (implementation) identity. Identity defaults to
# CLAUDE_CODE_SESSION_ID when `--author` is omitted from `tdd red`/`tdd green`
# (not just an honor-system string) — but a single agent can still defeat this
# by passing two different --author overrides; this is HOTL, not a hard
# security boundary. See the /tdd skill for details.
# strict_separation = false
"#;

#[cfg(test)]
mod violation_emission_tests {
    use super::*;

    // These mutate the process-global HOME env var (to isolate
    // `overwatch::store`'s home-relative storage root), so they share
    // `config::HOME_ENV_LOCK` with config.rs's workspace-trust test — see that
    // static's doc comment for why an unsynchronized $HOME mutation races
    // under the default multi-threaded test runner.
    fn with_scratch_home<T>(f: impl FnOnce(&Path) -> T) -> T {
        let _guard = config::HOME_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let home = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", home.path());
        let root = tempfile::tempdir().unwrap();
        let result = f(root.path());
        std::env::remove_var("HOME");
        result
    }

    #[test]
    fn emit_violation_records_a_tdd_event() {
        with_scratch_home(|root| {
            emit_violation(root, "sess-1", "missing-test");
            let events = overwatch::store::read_violations(root).expect("read_violations");
            assert_eq!(events.len(), 1, "expected exactly one recorded violation");
            assert_eq!(events[0].source, overwatch::violation::ViolationSource::Tdd);
            assert_eq!(events[0].signature, "tdd:missing-test");
        });
    }

    #[test]
    fn emit_violation_with_blank_check_kind_records_nothing() {
        with_scratch_home(|root| {
            emit_violation(root, "sess-1", "   ");
            let events = overwatch::store::read_violations(root).expect("read_violations");
            assert!(
                events.is_empty(),
                "a blank discriminator must not build a signature"
            );
        });
    }
}
