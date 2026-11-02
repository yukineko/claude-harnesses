//! sitecraft — CLI entry point. Exit codes: 0 clean, 1 violation,
//! 2 undetermined (see the library docs). `gate` speaks the Stop-hook protocol.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};

use sitecraft::{check, decide, gate, serve, Determination, EXIT_UNDETERMINED};

#[derive(Parser)]
#[command(name = "sitecraft", version)]
struct Cli {
    /// Project root (where `sitecraft.toml` and `sitecraft/decisions.toml`
    /// live). Defaults to the current directory. Ignored by `gate`, which
    /// uses the Stop payload's `cwd`.
    #[arg(long, global = true)]
    root: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Design-decision record (`sitecraft/decisions.toml`).
    Decide {
        #[command(subcommand)]
        cmd: DecideCmd,
    },
    /// Dev-server manager (runs the trusted `sitecraft.toml` command).
    Serve {
        #[command(subcommand)]
        cmd: ServeCmd,
    },
    /// Broken-page and security checks.
    Check {
        #[command(subcommand)]
        cmd: CheckCmd,
    },
    /// Stop hook gate (reads the Stop payload JSON on stdin).
    Gate,
    /// Record the current `sitecraft.toml` of the root as trusted.
    Trust,
}

#[derive(Subcommand)]
enum DecideCmd {
    /// Print the next unanswered question as JSON (or `{"status":"done"}`).
    Next,
    /// Record an answer. `--question relevance` with `relevant|irrelevant`
    /// judges the area; `--area domain` answers an added domain question.
    Record {
        #[arg(long)]
        area: String,
        #[arg(long)]
        question: String,
        #[arg(long)]
        answer: String,
    },
    /// Exit 0 when every required decision is recorded, 1 when some are
    /// pending, 2 when the record is absent or unreadable/invalid.
    Check,
    /// Add a domain-specific question derived from the purpose.
    AddQuestion {
        #[arg(long)]
        id: String,
        #[arg(long)]
        text: String,
        /// Allowed answer (repeatable). None = free text.
        #[arg(long = "choice")]
        choices: Vec<String>,
    },
    /// Classify a feature as `simple` or `dedicated-harness` and report
    /// whether a dedicated plugin already exists in the marketplace.
    Classify {
        #[arg(long)]
        feature: String,
        /// `simple` or `dedicated-harness`.
        #[arg(long)]
        kind: String,
        /// Plugin name of the dedicated harness (defaults to the feature).
        #[arg(long)]
        harness: Option<String>,
    },
}

#[derive(Subcommand)]
enum ServeCmd {
    /// Start the dev server (port check, process group, ready wait).
    Start,
    /// Stop the dev server's whole process group.
    Stop,
    /// Print the dev server status as JSON.
    Status,
}

#[derive(Subcommand)]
enum CheckCmd {
    /// Open the configured pages in Playwright and block on errors.
    Pages {
        /// Server origin; defaults to `http://127.0.0.1:<port>` from the config.
        #[arg(long)]
        base_url: Option<String>,
    },
    /// Scan build output for secrets, sources for HTML sinks, and run npm audit.
    Security,
}

fn main() {
    let cli = Cli::parse();
    let code = match cli.cmd {
        Cmd::Gate => gate::command(),
        cmd => match resolve_root(cli.root) {
            Ok(root) => dispatch(cmd, &root),
            Err(e) => {
                eprintln!("sitecraft: UNDETERMINED - {e}");
                EXIT_UNDETERMINED
            }
        },
    };
    std::process::exit(code);
}

fn resolve_root(root: Option<PathBuf>) -> Result<PathBuf, String> {
    match root {
        Some(r) => Ok(r),
        None => std::env::current_dir().map_err(|e| format!("cannot read the cwd: {e}")),
    }
}

fn dispatch(cmd: Cmd, root: &Path) -> i32 {
    match cmd {
        Cmd::Decide { cmd } => match cmd {
            DecideCmd::Next => decide::cmd_next(root),
            DecideCmd::Record {
                area,
                question,
                answer,
            } => decide::cmd_record(root, &area, &question, &answer),
            DecideCmd::Check => decide::cmd_check(root),
            DecideCmd::AddQuestion { id, text, choices } => {
                decide::cmd_add_question(root, &id, &text, choices)
            }
            DecideCmd::Classify {
                feature,
                kind,
                harness,
            } => decide::cmd_classify(root, &feature, &kind, harness),
        },
        Cmd::Serve { cmd } => with_state_dir(|state| match cmd {
            ServeCmd::Start => print_known("serve start", serve::start(state, root)),
            ServeCmd::Stop => print_known("serve stop", serve::stop(state, root)),
            ServeCmd::Status => print_known("serve status", serve::status(state, root)),
        }),
        Cmd::Check { cmd } => match cmd {
            CheckCmd::Pages { base_url } => sitecraft::report(
                "sitecraft check pages",
                &check::pages::run(root, base_url.as_deref()),
            ),
            CheckCmd::Security => {
                sitecraft::report("sitecraft check security", &check::security::run(root))
            }
        },
        Cmd::Trust => with_state_dir(|state| trust(state, root)),
        // Handled before root resolution.
        Cmd::Gate => gate::command(),
    }
}

/// Run `f` with the state dir; an unlocatable state dir is exit 2.
fn with_state_dir(f: impl FnOnce(&Path) -> i32) -> i32 {
    match sitecraft::state_dir() {
        Determination::Known(d) => f(&d),
        Determination::Undetermined(why) => {
            eprintln!("sitecraft: UNDETERMINED - {why}");
            EXIT_UNDETERMINED
        }
    }
}

/// `Known(v)` → `v` as JSON on stdout, exit 0; `Undetermined` → stderr, exit 2.
/// A value that cannot be encoded is exit 2 (an unprinted result is not success).
fn print_known<T: serde::Serialize>(label: &str, d: Determination<T>) -> i32 {
    match d {
        Determination::Known(v) => match serde_json::to_string(&v) {
            Ok(s) => {
                println!("{s}");
                0
            }
            Err(e) => {
                eprintln!("sitecraft {label}: UNDETERMINED - cannot encode the result: {e}");
                EXIT_UNDETERMINED
            }
        },
        Determination::Undetermined(why) => {
            eprintln!("sitecraft {label}: UNDETERMINED - {why}");
            EXIT_UNDETERMINED
        }
    }
}

fn trust(state: &Path, root: &Path) -> i32 {
    let cfg = sitecraft::config::path_for(root);
    match sitecraft::trust::record(state, root, &cfg) {
        Ok(h) => {
            println!("sitecraft: trusted {} (sha256 {h})", cfg.display());
            0
        }
        Err(e) => {
            eprintln!("sitecraft trust: UNDETERMINED - {e}");
            EXIT_UNDETERMINED
        }
    }
}
