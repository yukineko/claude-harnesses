//! `statusline` subcommand: a one-line progress readout for the most recent open
//! run, suitable for wiring into Claude Code's `statusLine` setting. Prints
//! nothing when there is no active run (the status line stays clean).
//!
//! Claude Code posts a JSON object on stdin for statusLine commands; we only
//! need `cwd`, and fall back to the process cwd if absent.

use crate::config::Config;
use crate::state::{self, Status};
use std::path::PathBuf;

pub fn run(cwd: &str) {
    // An empty status line reads as "no active run", so a configuration that
    // cannot be loaded (config.toml unreadable/unparseable: the configured
    // state_dir is unknown) is rendered as an explicit `unknown`, never as
    // silence or as whatever the default store holds.
    let cfg = match Config::load() {
        Ok(c) => c,
        Err(e) => {
            println!("condukt run state unknown ({e})");
            return;
        }
    };
    let cwd_path = if cwd.is_empty() {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    } else {
        PathBuf::from(cwd)
    };

    // An empty status line reads as "no active run", so a run-state store that
    // cannot even be located (undeterminable main worktree root) is rendered as
    // an explicit `unknown`, never as silence.
    let runs = match state::open_runs(&cfg, &cwd_path) {
        Ok(runs) => runs,
        Err(e) => {
            println!("condukt run state unknown ({e:#})");
            return;
        }
    };
    let Some(run) = runs.last() else {
        return;
    };

    let mut verified = 0;
    let mut running = 0;
    let mut failed = 0;
    for t in &run.tasks {
        match t.status {
            Status::Verified => verified += 1,
            Status::Running => running += 1,
            Status::Failed => failed += 1,
            _ => {}
        }
    }
    let total = run.tasks.len();
    let short: String = run.run_id.chars().take(8).collect();

    let mut parts = vec![format!("condukt {short} {verified}/{total} ok")];
    if running > 0 {
        parts.push(format!("{running} run"));
    }
    if failed > 0 {
        parts.push(format!("{failed} fail"));
    }
    println!("{}", parts.join(", "));
}
