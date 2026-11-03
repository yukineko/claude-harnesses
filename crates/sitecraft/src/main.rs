//! sitecraft — CLI entry point. Exit codes: 0 clean, 1 violation,
//! 2 undetermined (see the library docs). `gate` speaks the Stop-hook protocol.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};

use sitecraft::{capability, check, decide, gate, init, serve, Determination, EXIT_UNDETERMINED};

#[derive(Parser)]
#[command(name = "sitecraft", version)]
struct Cli {
    /// Project root (where `sitecraft.toml` and `sitecraft/decisions.toml`
    /// live). Defaults to the current directory. Ignored by `gate`, which
    /// uses the Stop payload's `cwd`, and by `init`, which takes `<dir>`.
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
        cmd: Box<DecideCmd>,
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
    /// Create a new site repository: `git init <dir>` plus `<dir>/sitecraft/`.
    /// Only a missing or empty directory outside any other git repository
    /// is used (else exit 1, nothing changed); git missing, failing or
    /// exceeding 30 s, or an IO failure is exit 2. Ignores `--root`.
    Init {
        /// The directory to create (or an existing empty one).
        dir: PathBuf,
    },
    /// Record the current `sitecraft.toml` of the root as trusted, and the
    /// `verify.argv` of every `command` capability in
    /// `sitecraft/decisions.toml` (key `<root>#capabilities`; removed when
    /// there is none). Run by a human only.
    Trust,
    /// Capabilities beyond the web app (`[capabilities.<name>]`).
    Capability {
        #[command(subcommand)]
        cmd: CapabilityCmd,
    },
}

#[derive(Subcommand)]
enum CapabilityCmd {
    /// Whether the capability can run on this machine: prints one JSON line
    /// `{"capability","result","reason"}`; exit 0 feasible, 1 infeasible,
    /// 2 undetermined (120 s limit). Writes nothing. Not completion evidence.
    Probe {
        /// The capability name.
        name: String,
    },
}

#[derive(Subcommand)]
enum DecideCmd {
    /// Print the next unanswered question as JSON (or `{"status":"done"}`).
    Next,
    /// Record an answer. `--question relevance` with `relevant|irrelevant`
    /// judges the area; `--area domain` answers an added domain question.
    /// Repeat `--answer` for a multi-select question; `--note` stores a
    /// remark (omitting it keeps an existing note). A changed value prints
    /// `changed: <old> -> <new>` and is appended to `[[history]]`.
    Record {
        #[arg(long)]
        area: String,
        #[arg(long)]
        question: String,
        #[arg(long = "answer", required = true)]
        answers: Vec<String>,
        #[arg(long)]
        note: Option<String>,
    },
    /// Record the purpose summary (`[purpose] text`). A blank text is exit 1.
    Purpose {
        #[arg(long)]
        text: String,
    },
    /// Exit 0 when the purpose and every required decision are recorded and
    /// no consistency rule is violated, 1 when something is pending or
    /// violated, 2 when the record is absent or unreadable/invalid. Warnings
    /// go to stderr and do not change the exit code.
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
        /// Allow several answers (stored as an array).
        #[arg(long)]
        multi: bool,
    },
    /// Classify a feature as `simple` or `dedicated-harness` and report
    /// whether a dedicated plugin already exists in the marketplace, its
    /// description, and whether its reuse is confirmed by the user.
    Classify {
        #[arg(long)]
        feature: String,
        /// `simple` or `dedicated-harness`.
        #[arg(long)]
        kind: String,
        /// Plugin name of the dedicated harness (defaults to the feature).
        #[arg(long)]
        harness: Option<String>,
        /// The user's answer (`yes`/`no`) to whether the existing plugin fits
        /// the purpose. Only accepted when the plugin exists.
        #[arg(long = "confirm-fit")]
        confirm_fit: Option<String>,
    },
    /// Record (or replace) `[capabilities.<name>]`. A relevant capability
    /// needs `--how`, `--done-when` and `--verify-kind`
    /// (`builtin:blender-headless` with `--script --glb --render`,
    /// `builtin:threejs-render` with `--page [--selector]`, `command` with
    /// repeated `--argv` and `--input`). Invalid input is exit 2 and changes
    /// nothing. A changed value is appended to `[[history]]`.
    Capability {
        #[arg(long)]
        name: String,
        /// `relevant` or `irrelevant`.
        #[arg(long)]
        relevance: String,
        #[arg(long)]
        how: Option<String>,
        #[arg(long = "done-when")]
        done_when: Option<String>,
        #[arg(long = "verify-kind")]
        verify_kind: Option<String>,
        #[arg(long)]
        script: Option<String>,
        #[arg(long)]
        glb: Option<String>,
        #[arg(long)]
        render: Option<String>,
        #[arg(long)]
        page: Option<String>,
        #[arg(long)]
        selector: Option<String>,
        /// One argv element of a `command` capability (repeat, in order).
        #[arg(long = "argv", allow_hyphen_values = true)]
        argv: Vec<String>,
        /// One input glob of a `command` (or builtin) capability (repeatable).
        #[arg(long = "input")]
        inputs: Vec<String>,
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
    /// Run a relevant capability's verify (300 s limit) and write its result
    /// record: exit 0 clean, 1 violation, 2 undetermined.
    Capability {
        /// The capability name.
        name: String,
    },
}

fn main() {
    let cli = Cli::parse();
    let code = match cli.cmd {
        Cmd::Gate => gate::command(),
        Cmd::Init { dir } => init::command(&dir),
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
        Cmd::Decide { cmd } => match *cmd {
            DecideCmd::Next => decide::cmd_next(root),
            DecideCmd::Record {
                area,
                question,
                answers,
                note,
            } => decide::cmd_record(root, &area, &question, &answers, note.as_deref()),
            DecideCmd::Purpose { text } => decide::cmd_purpose(root, &text),
            DecideCmd::Check => decide::cmd_check(root),
            DecideCmd::AddQuestion {
                id,
                text,
                choices,
                multi,
            } => decide::cmd_add_question(root, &id, &text, choices, multi),
            DecideCmd::Classify {
                feature,
                kind,
                harness,
                confirm_fit,
            } => decide::cmd_classify(root, &feature, &kind, harness, confirm_fit.as_deref()),
            DecideCmd::Capability {
                name,
                relevance,
                how,
                done_when,
                verify_kind,
                script,
                glb,
                render,
                page,
                selector,
                argv,
                inputs,
            } => decide::cmd_capability(
                root,
                &name,
                &decide::capability::CapabilityArgs {
                    relevance,
                    how,
                    done_when,
                    verify_kind,
                    script,
                    glb,
                    render,
                    page,
                    selector,
                    argv,
                    inputs,
                },
            ),
        },
        Cmd::Capability { cmd } => match cmd {
            CapabilityCmd::Probe { name } => capability::cmd_probe(root, &name),
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
            CheckCmd::Capability { name } => capability::cmd_check(root, &name),
        },
        Cmd::Trust => with_state_dir(|state| trust(state, root)),
        // Handled before root resolution.
        Cmd::Gate => gate::command(),
        Cmd::Init { dir } => init::command(&dir),
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

/// `sitecraft trust`: the `sitecraft.toml` hash and, in the same write, the
/// D2-trust digest of the `command` capabilities (none → the
/// `#capabilities` key is removed). An unusable decision record is exit 2
/// and nothing is written (the digest would otherwise be guessed).
fn trust(state: &Path, root: &Path) -> i32 {
    let cfg = sitecraft::config::path_for(root);
    let digest = match decide::record::load(root) {
        Determination::Known(None) => None,
        Determination::Known(Some(d)) => match d.parsed_capabilities() {
            Ok(caps) => decide::capability::command_digest(&caps),
            Err(e) => {
                eprintln!(
                    "sitecraft trust: UNDETERMINED - the capabilities in {} are unusable ({e}); nothing was trusted",
                    decide::record::path_for(root).display()
                );
                return EXIT_UNDETERMINED;
            }
        },
        Determination::Undetermined(why) => {
            eprintln!("sitecraft trust: UNDETERMINED - {why}; nothing was trusted");
            return EXIT_UNDETERMINED;
        }
    };
    match sitecraft::trust::record_with_capabilities(state, root, &cfg, digest.as_deref()) {
        Ok(h) => {
            println!("sitecraft: trusted {} (sha256 {h})", cfg.display());
            if let Some(d) = &digest {
                println!("sitecraft: trusted the command capabilities' argv (sha256 {d})");
            }
            0
        }
        Err(e) => {
            eprintln!("sitecraft trust: UNDETERMINED - {e}");
            EXIT_UNDETERMINED
        }
    }
}
