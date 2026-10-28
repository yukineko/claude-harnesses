//! webqa — Web QA gate plugin (Playwright).
mod config;
mod gate;
mod git;
mod report;
mod weaken;

use clap::{Parser, Subcommand};
use std::io::Read;
use std::path::PathBuf;

use harness_core::hook::HookInput;

#[derive(Parser)]
#[command(name = "webqa", version)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Stop hook gate: run the Playwright specs mapped to the touched web files;
    /// block the stop until they pass.
    Gate,
    /// Detect test-weakening edits to Playwright specs.
    /// Exit 0 clean, 1 findings, 2 undetermined.
    WeakenCheck {
        /// Read a unified diff from this file instead of `git diff --cached`.
        #[arg(long)]
        diff_file: Option<PathBuf>,
    },
}

fn main() {
    let cli = Cli::parse();
    let code = match cli.cmd {
        Cmd::Gate => gate_command(),
        Cmd::WeakenCheck { diff_file } => weaken::report(&weaken::run(diff_file.as_deref())),
    };
    std::process::exit(code);
}

/// Per-session attempt-counter / scratch dir: `$WEBQA_STATE_DIR`, else `~/.webqa`.
fn state_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("WEBQA_STATE_DIR") {
        return PathBuf::from(d);
    }
    match std::env::var_os("HOME") {
        Some(h) => PathBuf::from(h).join(".webqa"),
        None => std::env::temp_dir().join("webqa-state"),
    }
}

fn gate_command() -> ! {
    let mut raw = String::new();
    let _ = std::io::stdin().read_to_string(&mut raw);
    let hook = HookInput::parse(&raw);
    let interactive = hook.is_none();
    let active = hook.as_ref().is_some_and(|h| h.stop_hook_active);
    // Panic barrier: fail-closed (a crashed gate blocks), bounded per session.
    harness_core::gate::run::run_guarded("webqa", interactive, active, move || gate_run(hook))
}

fn gate_run(hook: Option<HookInput>) -> ! {
    let interactive = hook.is_none();
    let input = hook.unwrap_or_default();
    let root = input.cwd_or_current();
    let session = input.session_key();
    let dir = state_dir();

    let cfg = match config::load(&root) {
        config::Loaded::Absent => {
            eprintln!(
                "webqa: not configured ({} not found in {}); nothing to check",
                config::FILE_NAME,
                root.display()
            );
            std::process::exit(0);
        }
        config::Loaded::Config(c) => *c,
        config::Loaded::Undetermined(why) => {
            // Cannot read the declaration: judge it undetermined, bounded like
            // any other block (default attempt budget).
            let v = harness_core::verdict::Verdict::undetermined(why);
            finish(&v, config_defaults(), &dir, &session, interactive)
        }
    };

    let v = gate::evaluate(&cfg, &root, &dir.join("runs"));
    finish(
        &v,
        (cfg.max_attempts, cfg.reset_after_secs),
        &dir,
        &session,
        interactive,
    )
}

fn config_defaults() -> (u32, i64) {
    (3, 1800)
}

fn finish(
    v: &harness_core::verdict::Verdict,
    budget: (u32, i64),
    dir: &std::path::Path,
    session: &str,
    interactive: bool,
) -> ! {
    use harness_core::gate::state;
    use harness_core::verdict::Verdict;
    let (max_attempts, reset_after) = budget;
    let text = match v {
        Verdict::Clean(_) => {
            state::reset(dir, session);
            std::process::exit(0);
        }
        Verdict::Violation(r) => format!("webqa: BLOCKED - {}", r.as_str()),
        Verdict::Undetermined(u) => {
            format!("webqa: BLOCKED - undetermined: {}", u.as_str())
        }
    };
    let attempt = state::bump(dir, session, reset_after);
    if interactive {
        eprintln!("{text}");
        std::process::exit(1);
    }
    if attempt > max_attempts {
        state::reset(dir, session);
        eprintln!("{text}");
        println!(
            "{}",
            serde_json::json!({
                "systemMessage": format!(
                    "webqa gave up after {max_attempts} blocked stops and is allowing this stop. \
                     NOTHING WAS VERIFIED: the web QA is still failing or undetermined. Last result: {text}"
                )
            })
        );
        std::process::exit(0);
    }
    println!(
        "{}",
        serde_json::json!({
            "decision": "block",
            "reason": format!("{text}\n(attempt {attempt}/{max_attempts}; fix the cause and stop again)"),
        })
    );
    std::process::exit(0);
}
