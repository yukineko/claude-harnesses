//! difflog — session diff-log for Claude Code.
//!
//! SessionStart: snapshot the HEAD SHA.
//! SessionEnd:   generate a structured git diff summary (commits, stat, files
//!               changed, bounded diff body) and write it to the log directory.
//!
//! A /difflog skill can then have an LLM read the log and produce a human-
//! readable narrative (89% vs 62% developer acceptance rate difference).

mod config;
mod git;
mod install;
mod record;
mod state;

use clap::{Parser, Subcommand};

use harness_core::hook::{read_stdin, read_stdin_if_piped, run_hook};

use config::Config;

#[derive(Parser)]
#[command(
    name = "difflog",
    version,
    about = "Session diff-log for Claude Code: snapshot HEAD at session start, write a structured git diff summary at session end."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// SessionStart hook: snapshot HEAD SHA.
    SessionStart,
    /// SessionEnd hook: write the diff-log.
    SessionEnd,
    /// List recent diff-logs.
    List {
        #[arg(long, default_value = "10")]
        limit: usize,
    },
    /// Print the contents of the most recent diff-log.
    Last,
    /// Merge SessionStart + SessionEnd hooks into ~/.claude/settings.json.
    Install {
        #[arg(long)]
        dry_run: bool,
    },
    /// Remove the hooks from ~/.claude/settings.json.
    Uninstall {
        #[arg(long)]
        dry_run: bool,
    },
    /// Write a starter ./difflog.toml.
    Init {
        #[arg(long)]
        force: bool,
    },
    /// Show the resolved config.
    Status,
}

fn main() {
    let cli = Cli::parse();
    match cli.command {
        Command::SessionStart => run_hook(session_start),
        Command::SessionEnd => run_hook(session_end),
        Command::List { limit } => list(limit),
        Command::Last => last(),
        Command::Install { dry_run } => exit_on_err(install::install(dry_run)),
        Command::Uninstall { dry_run } => exit_on_err(install::uninstall(dry_run)),
        Command::Init { force } => exit_on_err(init(force)),
        Command::Status => status(),
    }
}

fn exit_on_err(r: anyhow::Result<()>) {
    if let Err(e) = r {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn session_start() {
    if Config::disabled_env() {
        return;
    }
    let raw = read_stdin();
    let Some(input) = harness_core::hook::HookInput::parse(&raw) else {
        return;
    };
    if input.session_id.is_empty() {
        return;
    }
    let cwd = input.cwd_or_current();
    let cfg = Config::load(&cwd);
    if cfg.enabled {
        record::on_session_start(&input, &cfg);
    }
}

fn session_end() {
    if Config::disabled_env() {
        return;
    }
    let raw = read_stdin_if_piped();
    let Some(input) = harness_core::hook::HookInput::parse(&raw) else {
        return;
    };
    if input.session_id.is_empty() {
        return;
    }
    let cwd = input.cwd_or_current();
    let cfg = Config::load(&cwd);
    if cfg.enabled {
        record::on_session_end(&input, &cfg);
    }
}

/// Reads `dir` and returns its `.md` entries, warning (not silently dropping)
/// on any error — a missing `log_dir` prints "not found"; an existing-but-
/// unreadable one, or an unreadable individual entry, prints a distinct
/// warning so the two cannot-determine cases are never confused with "no logs
/// yet".
fn md_entries(dir: &std::path::Path) -> Vec<std::fs::DirEntry> {
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("log_dir not found: {}", dir.display());
            return Vec::new();
        }
        Err(e) => {
            eprintln!("log_dir unreadable: {}: {e}", dir.display());
            return Vec::new();
        }
    };
    let mut entries = Vec::new();
    for e in rd {
        match e {
            Ok(e) => {
                if e.path().extension().map(|x| x == "md").unwrap_or(false) {
                    entries.push(e);
                }
            }
            Err(e) => eprintln!("warning: unreadable entry in {}: {e}", dir.display()),
        }
    }
    entries
}

fn list(limit: usize) {
    let cfg = Config::load(&std::env::current_dir().unwrap_or_else(|_| ".".into()));
    let dir = &cfg.log_dir;
    let mut entries = md_entries(dir);
    entries.sort_by_key(|e| std::cmp::Reverse(e.file_name()));
    for e in entries.iter().take(limit) {
        println!("{}", e.file_name().to_string_lossy());
    }
}

fn last() {
    let cfg = Config::load(&std::env::current_dir().unwrap_or_else(|_| ".".into()));
    let dir = &cfg.log_dir;
    let mut entries = md_entries(dir);
    entries.sort_by_key(|e| std::cmp::Reverse(e.file_name()));
    if let Some(e) = entries.first() {
        match std::fs::read_to_string(e.path()) {
            Ok(s) => print!("{s}"),
            Err(err) => eprintln!("error: {err}"),
        }
    } else {
        eprintln!("no diff-logs found in {}", dir.display());
    }
}

fn init(force: bool) -> anyhow::Result<()> {
    let path = std::path::PathBuf::from("difflog.toml");
    if path.exists() && !force {
        eprintln!("difflog.toml already exists — pass --force to overwrite");
        return Ok(());
    }
    let template = include_str!("../difflog.example.toml");
    std::fs::write(&path, template)?;
    eprintln!("wrote {}", path.display());
    Ok(())
}

fn status() {
    let cfg = Config::load(&std::env::current_dir().unwrap_or_else(|_| ".".into()));
    println!("enabled:          {}", cfg.enabled);
    println!("log_dir:          {}", cfg.log_dir.display());
    println!("diff_body_limit:  {} bytes", cfg.diff_body_limit);
    println!("exclude_globs:    {:?}", cfg.exclude_globs);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn md_entries_absent_dir_returns_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("does-not-exist");
        assert!(md_entries(&missing).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn md_entries_does_not_panic_on_unreadable_dir() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.md"), "hi").unwrap();
        let mut perms = std::fs::metadata(dir.path()).unwrap().permissions();
        perms.set_mode(0o000);
        std::fs::set_permissions(dir.path(), perms.clone()).unwrap();
        let result = std::panic::catch_unwind(|| md_entries(dir.path()));
        perms.set_mode(0o755);
        std::fs::set_permissions(dir.path(), perms).unwrap();
        assert!(
            result.is_ok(),
            "md_entries must not panic on an unreadable dir"
        );
        assert!(result.unwrap().is_empty());
    }

    #[test]
    fn md_entries_filters_to_md_only() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.md"), "hi").unwrap();
        std::fs::write(dir.path().join("b.txt"), "hi").unwrap();
        let entries = md_entries(dir.path());
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].file_name().to_string_lossy(), "a.md");
    }
}
