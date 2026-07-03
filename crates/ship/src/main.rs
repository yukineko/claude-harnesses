//! ship — nudge the commit・merge・push・plugin-update shipping ritual.
//!
//! Hard invariant: this binary NEVER invokes a state-mutating git command
//! (commit/merge/push/add/checkout/reset). It only *detects* and *reports*
//! unshipped state; the two runnable steps are `scripts/rebuild-plugins.sh`
//! via `ship check --run-safe` (binary swap into existing cache dirs) and
//! `scripts/rollout-plugins.sh` via `ship rollout` (heavier, explicit: the
//! `/plugin update` step — advances the installed version pointer). Neither
//! touches git. See `tests/integration.rs::binary_never_mutates_git`.

mod checklist;
mod git;
mod stale;

use std::path::{Path, PathBuf};
use std::process::Command;

use clap::{Parser, Subcommand};

use checklist::ShipStatus;
use harness_core::hook::{run_hook, HookInput};

#[derive(Parser)]
#[command(name = "ship")]
#[command(about = "nudge the commit・merge・push・plugin-update shipping ritual")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Detect unshipped state and print a checklist.
    Check {
        /// Also run the ONE safe/auto-runnable step (scripts/rebuild-plugins.sh),
        /// then reprint the remaining GATED items. Never runs git.
        #[arg(long)]
        run_safe: bool,
    },
    /// SessionEnd hook: print a reminder to stdout only if unshipped work
    /// exists. Always exits 0, even on malformed/empty stdin.
    SessionEnd,
    /// Run `scripts/rollout-plugins.sh` — automates the `/plugin update` step
    /// (creates the cache `<name>/<version>/` dir, repoints
    /// `installed_plugins.json`, then rebuild+sync). A heavier, explicit
    /// operation kept SEPARATE from `check --run-safe`: it mutates
    /// `~/.claude/plugins/` state, never git. This is what fully clears
    /// "stale plugin binaries" that `--run-safe` alone can't.
    Rollout,
}

fn main() {
    let cli = Cli::parse();
    match cli.command {
        Cmd::Check { run_safe } => check(run_safe),
        Cmd::SessionEnd => run_hook(session_end),
        Cmd::Rollout => rollout(),
    }
}

/// Resolve the repo root: walk up from the current dir to the git toplevel.
/// Falls back to the current dir if that fails (fail-soft; detections
/// themselves are fail-soft too).
fn resolve_repo_root(start: &Path) -> PathBuf {
    let output = Command::new("git")
        .arg("rev-parse")
        .arg("--show-toplevel")
        .current_dir(start)
        .output();

    match output {
        Ok(o) if o.status.success() => {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if s.is_empty() {
                start.to_path_buf()
            } else {
                PathBuf::from(s)
            }
        }
        _ => start.to_path_buf(),
    }
}

fn detect(repo: &Path) -> ShipStatus {
    ShipStatus {
        uncommitted: git::uncommitted_changes(repo),
        unmerged_branches: git::unmerged_condukt_branches(repo),
        leftover_worktrees: git::leftover_worktrees(repo),
        unpushed_count: git::unpushed_count(repo),
        stale_crates: stale::stale_crates(repo),
    }
}

fn check(run_safe: bool) {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let repo = resolve_repo_root(&cwd);

    let status = detect(&repo);
    print!("{}", checklist::render(&status));

    if run_safe {
        println!();
        match checklist::run_safe(&repo) {
            Ok(summary) => print!("{summary}"),
            Err(e) => eprintln!("[run-safe] error: {e}"),
        }

        // Re-detect (rebuild may have changed stale-crate state) and reprint
        // the remaining GATED items.
        let status = detect(&repo);
        println!();
        println!("remaining GATED items:");
        print!("{}", checklist::render_gated_remaining(&status));
    }
}

fn rollout() {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let repo = resolve_repo_root(&cwd);

    match checklist::rollout(&repo) {
        Ok(summary) => print!("{summary}"),
        Err(e) => eprintln!("[rollout] error: {e}"),
    }
}

fn session_end() {
    let raw = harness_core::hook::read_stdin();
    let Some(input) = HookInput::parse(&raw) else {
        return;
    };
    let cwd = input.cwd_or_current();
    let repo = resolve_repo_root(&cwd);
    let status = detect(&repo);

    if let Some(reminder) = checklist::session_end_reminder(&status) {
        println!("{reminder}");
    }
}
