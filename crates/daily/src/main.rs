//! daily — run registered tasks once per calendar day at SessionStart.
//!
//! Time-based (cron) firing is fragile across machines/shells, so `daily`
//! instead gates on the *first session of each calendar day*: the SessionStart
//! hook checks a per-task `DailyGuard` and runs anything not yet run today.
//!
//! Tasks are **registered** in `~/.daily/config.toml` as a `[[task]]` array
//! (name + shell command, optional `dir`). With no tasks configured, a default
//! `security` task (`cargo deny check …`) runs, preserving the original
//! behavior. Each task is keyed independently, state in
//! `~/.daily/state/<name>-daily.txt` via `DailyGuard`.
//!
//! The runner is a deterministic Rust binary — it never calls an LLM. It runs
//! each due task as a subprocess and injects a one-line summary of what ran
//! (`📋 daily: ran a (ok), b (fail exit 1: …)`) as `additionalContext`. It
//! always exits 0 (never breaks a turn).

use clap::{Parser, Subcommand};
use harness_core::daily::DailyGuard;
use harness_core::hook::{read_stdin, run_hook, HookInput};
use serde::Deserialize;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Parser)]
#[command(
    name = "daily",
    version,
    about = "Run registered tasks once per calendar day at SessionStart"
)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// SessionStart hook: run any registered task not yet run today.
    SessionStart,
    /// List registered tasks and whether each has already run today.
    List,
    /// Register a new daily task in ~/.daily/config.toml.
    Add {
        /// Unique task name (also the once-per-day state key).
        #[arg(long)]
        name: String,
        /// Shell command to run (executed via `sh -c`).
        #[arg(long)]
        command: String,
        /// Working directory (defaults to the session cwd at run time).
        #[arg(long)]
        dir: Option<String>,
    },
    /// Install the SessionStart hook into ~/.claude/settings.json (not yet implemented).
    Install,
}

fn main() {
    let cli = Cli::parse();
    match cli.command {
        Cmd::SessionStart => session_start_cmd(),
        Cmd::List => list_cmd(),
        Cmd::Add { name, command, dir } => add_cmd(&name, &command, dir.as_deref()),
        Cmd::Install => {
            eprintln!("daily install: not yet implemented — add the hook manually to ~/.claude/settings.json");
            std::process::exit(0);
        }
    }
}

// ─────────────────────────── config / tasks ────────────────────────────────

/// A registered daily task: a named shell command, optionally pinned to a dir.
#[derive(Debug, Clone, Deserialize, PartialEq)]
struct Task {
    name: String,
    command: String,
    #[serde(default)]
    dir: Option<String>,
}

/// `~/.daily/config.toml` shape. `enabled` defaults to true (a missing config
/// means enabled); `[[task]]` entries populate `task`.
#[derive(Debug, Clone, Deserialize)]
struct Config {
    #[serde(default = "default_true")]
    enabled: bool,
    #[serde(default)]
    task: Vec<Task>,
}

fn default_true() -> bool {
    true
}

impl Default for Config {
    fn default() -> Self {
        Config {
            enabled: true,
            task: Vec::new(),
        }
    }
}

/// Parse config TOML text. Malformed TOML falls back to the default (enabled,
/// no tasks) so a broken config never breaks a turn — the error goes to stderr.
fn parse_config(text: &str) -> Config {
    match toml::from_str::<Config>(text) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("daily: ignoring unparseable ~/.daily/config.toml: {e}");
            Config::default()
        }
    }
}

fn load_config() -> Config {
    match std::fs::read_to_string(config_path()) {
        Ok(text) => parse_config(&text),
        Err(_) => Config::default(),
    }
}

/// The default `security` task, used when the config registers no tasks (keeps
/// the original once-per-day cargo-deny audit working out of the box).
fn default_security_task() -> Task {
    Task {
        name: "security".to_string(),
        command: "cargo deny check advisories bans sources licenses".to_string(),
        dir: None,
    }
}

/// The tasks to consider: the registered ones, or the default security task
/// when none are registered.
fn effective_tasks(config: &Config) -> Vec<Task> {
    if config.task.is_empty() {
        vec![default_security_task()]
    } else {
        config.task.clone()
    }
}

// ─────────────────────────── SessionStart ──────────────────────────────────

fn session_start_cmd() -> ! {
    run_hook(|| {
        let raw = read_stdin();
        let input = HookInput::parse(&raw).unwrap_or_default();

        let config = load_config();
        if !config.enabled {
            return;
        }

        let state_dir = daily_state_dir();
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        let cwd = input.cwd_or_current();

        // Run every task not yet run today, keyed independently. mark_done()
        // runs regardless of outcome — one attempt per calendar day.
        let mut results: Vec<(String, Outcome)> = Vec::new();
        for task in effective_tasks(&config) {
            let guard = DailyGuard::new(&state_dir, &task.name, &today);
            if !guard.should_run() {
                continue;
            }
            let outcome = run_task(&task, &cwd);
            guard.mark_done().ok();
            results.push((task.name, outcome));
        }

        // Per the chosen behavior: always inject a summary of what ran today
        // (both ok and fail). Nothing ran → stay silent.
        if let Some(msg) = summary(&results) {
            println!("{}", json!({ "additionalContext": msg }));
        }
    })
}

/// The result of attempting one registered task.
#[derive(Debug)]
enum Outcome {
    /// Exited 0.
    Ok,
    /// Exited non-zero (or was killed): the code (if any) plus a brief.
    Failed { code: Option<i32>, brief: String },
    /// The command could not be spawned at all (e.g. `sh` missing).
    SpawnError(String),
}

/// Run one task via `sh -c`, in its `dir` (or the session cwd), with
/// `$CARGO_HOME/bin` prepended to PATH so cargo subcommands resolve even when
/// `~/.cargo/bin` isn't on the ambient PATH.
fn run_task(task: &Task, cwd: &Path) -> Outcome {
    let dir = task
        .dir
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(|| cwd.to_path_buf());

    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(&task.command).current_dir(&dir);
    if let Some(path) = augmented_path() {
        cmd.env("PATH", path);
    }

    match cmd.output() {
        Ok(out) if out.status.success() => Outcome::Ok,
        Ok(out) => {
            let combined = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            Outcome::Failed {
                code: out.status.code(),
                brief: summarize_output(&combined),
            }
        }
        Err(e) => Outcome::SpawnError(e.to_string()),
    }
}

/// Build the one-line summary injected as `additionalContext`, or `None` when
/// no task ran (stay silent). Uses ⚠️ when any task failed, 📋 otherwise.
fn summary(results: &[(String, Outcome)]) -> Option<String> {
    if results.is_empty() {
        return None;
    }
    let parts: Vec<String> = results
        .iter()
        .map(|(name, o)| match o {
            Outcome::Ok => format!("{name} (ok)"),
            Outcome::Failed { code, brief } => {
                let status = match code {
                    Some(c) => format!("exit {c}"),
                    None => "killed".to_string(),
                };
                format!("{name} (fail {status}: {})", first_line(brief))
            }
            Outcome::SpawnError(e) => format!("{name} (error: {})", first_line(e)),
        })
        .collect();
    let any_fail = results.iter().any(|(_, o)| !matches!(o, Outcome::Ok));
    let icon = if any_fail { "⚠️" } else { "📋" };
    Some(format!("{icon} daily: ran {}", parts.join(", ")))
}

// ─────────────────────────── list / add ────────────────────────────────────

fn list_cmd() -> ! {
    let config = load_config();
    let tasks = effective_tasks(&config);
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let state_dir = daily_state_dir();

    let state = if config.enabled {
        "enabled"
    } else {
        "disabled"
    };
    println!("daily tasks ({}, {} registered):", state, config.task.len());
    for t in &tasks {
        let guard = DailyGuard::new(&state_dir, &t.name, &today);
        let ran = if guard.should_run() {
            "pending today"
        } else {
            "ran today"
        };
        let where_ = t
            .dir
            .as_deref()
            .map(|d| format!(" @{d}"))
            .unwrap_or_default();
        println!("  - {} [{}]{}  $ {}", t.name, ran, where_, t.command);
    }
    if config.task.is_empty() {
        println!("(no tasks registered; showing the built-in default. `daily add` to register your own.)");
    }
    std::process::exit(0);
}

fn add_cmd(name: &str, command: &str, dir: Option<&str>) -> ! {
    let path = config_path();
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            eprintln!("daily add: cannot create {}: {e}", parent.display());
            std::process::exit(1);
        }
    }

    // Reject a duplicate name so two tasks can't share one daily state key.
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let config = parse_config(&existing);
    if config.task.iter().any(|t| t.name == name) {
        eprintln!("daily add: a task named '{name}' is already registered");
        std::process::exit(1);
    }

    // Append a [[task]] block textually so existing content/comments survive.
    let block = render_task_block(name, command, dir);
    let mut body = existing;
    if !body.is_empty() && !body.ends_with('\n') {
        body.push('\n');
    }
    body.push_str(&block);
    if let Err(e) = std::fs::write(&path, &body) {
        eprintln!("daily add: cannot write {}: {e}", path.display());
        std::process::exit(1);
    }
    println!("daily: registered task '{name}' → runs once/day at SessionStart");
    std::process::exit(0);
}

/// Render an appendable `[[task]]` TOML block with basic-string escaping.
fn render_task_block(name: &str, command: &str, dir: Option<&str>) -> String {
    let mut s = format!(
        "\n[[task]]\nname = \"{}\"\ncommand = \"{}\"\n",
        toml_escape(name),
        toml_escape(command)
    );
    if let Some(d) = dir {
        s.push_str(&format!("dir = \"{}\"\n", toml_escape(d)));
    }
    s
}

/// Escape a value for a TOML basic string (`"…"`): backslash and quote.
fn toml_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

// ─────────────────────────── helpers ───────────────────────────────────────

/// Extract the first few salient lines (error/warning/RUSTSEC), falling back to
/// the first lines when none match — used to summarize a failed task's output.
fn summarize_output(output: &str) -> String {
    let lines: Vec<&str> = output
        .lines()
        .filter(|l| l.contains("error") || l.contains("warning") || l.contains("RUSTSEC"))
        .take(5)
        .collect();
    if lines.is_empty() {
        output.lines().take(5).collect::<Vec<_>>().join("\n")
    } else {
        lines.join("\n")
    }
}

/// First non-empty line of `s`, trimmed and truncated to keep the summary tidy.
fn first_line(s: &str) -> String {
    let line = s
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    if line.chars().count() > 160 {
        let truncated: String = line.chars().take(157).collect();
        format!("{truncated}…")
    } else {
        line.to_string()
    }
}

/// PATH with `$CARGO_HOME/bin` (or `~/.cargo/bin`) prepended, so cargo
/// subcommands resolve even when the shell PATH lacks it. `None` when there's
/// nothing to add.
fn augmented_path() -> Option<String> {
    let cargo_home = std::env::var("CARGO_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| dirs::home_dir().unwrap_or_default().join(".cargo"));
    let cargo_bin = cargo_home.join("bin");
    if !cargo_bin.exists() {
        return None;
    }
    let existing = std::env::var("PATH").unwrap_or_default();
    Some(match existing.is_empty() {
        true => cargo_bin.to_string_lossy().into_owned(),
        false => format!("{}:{}", cargo_bin.to_string_lossy(), existing),
    })
}

fn daily_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".daily")
}

fn daily_state_dir() -> PathBuf {
    daily_dir().join("state")
}

fn config_path() -> PathBuf {
    daily_dir().join("config.toml")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults_to_enabled_with_no_tasks() {
        let c = Config::default();
        assert!(c.enabled);
        assert!(c.task.is_empty());
    }

    #[test]
    fn parse_config_reads_enabled_false() {
        let c = parse_config("enabled = false\n");
        assert!(!c.enabled);
    }

    #[test]
    fn parse_config_missing_enabled_defaults_true() {
        // A config that only registers a task is still enabled.
        let c = parse_config("[[task]]\nname = \"x\"\ncommand = \"echo hi\"\n");
        assert!(c.enabled);
        assert_eq!(c.task.len(), 1);
        assert_eq!(c.task[0].name, "x");
        assert_eq!(c.task[0].command, "echo hi");
        assert!(c.task[0].dir.is_none());
    }

    #[test]
    fn parse_config_reads_task_array_with_dir() {
        let text = "\
[[task]]
name = \"a\"
command = \"cmd-a\"

[[task]]
name = \"b\"
command = \"cmd-b\"
dir = \"/repo\"
";
        let c = parse_config(text);
        assert_eq!(c.task.len(), 2);
        assert_eq!(c.task[1].dir.as_deref(), Some("/repo"));
    }

    #[test]
    fn parse_config_malformed_falls_back_to_default() {
        let c = parse_config("this is not = valid toml ][");
        assert!(c.enabled);
        assert!(c.task.is_empty());
    }

    #[test]
    fn effective_tasks_seeds_security_when_empty() {
        let tasks = effective_tasks(&Config::default());
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].name, "security");
        assert!(tasks[0].command.contains("cargo deny check"));
    }

    #[test]
    fn effective_tasks_uses_registered_when_present() {
        let c = parse_config("[[task]]\nname = \"only\"\ncommand = \"true\"\n");
        let tasks = effective_tasks(&c);
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].name, "only");
    }

    #[test]
    fn summary_is_none_when_nothing_ran() {
        assert!(summary(&[]).is_none());
    }

    #[test]
    fn summary_all_ok_uses_clipboard_icon_and_lists_ok() {
        let msg = summary(&[("sec".to_string(), Outcome::Ok)]).unwrap();
        assert!(msg.starts_with("📋 daily: ran"));
        assert!(msg.contains("sec (ok)"));
        assert!(!msg.contains("fail"));
    }

    #[test]
    fn summary_with_failure_uses_warning_icon_and_brief() {
        let results = vec![
            ("a".to_string(), Outcome::Ok),
            (
                "b".to_string(),
                Outcome::Failed {
                    code: Some(1),
                    brief: "error[E]: broke\nother".to_string(),
                },
            ),
        ];
        let msg = summary(&results).unwrap();
        assert!(msg.starts_with("⚠️"));
        assert!(msg.contains("a (ok)"));
        assert!(msg.contains("b (fail exit 1: error[E]: broke)"));
    }

    #[test]
    fn summary_spawn_error_is_reported_distinctly() {
        let msg = summary(&[(
            "x".to_string(),
            Outcome::SpawnError("No such file".to_string()),
        )])
        .unwrap();
        assert!(msg.contains("x (error: No such file)"));
    }

    #[test]
    fn summarize_picks_error_lines() {
        let output =
            "some preamble\nerror[A001]: advisory blah\nwarning: license issue\nignored line";
        let s = summarize_output(output);
        assert!(s.contains("error") || s.contains("warning"));
    }

    #[test]
    fn summarize_falls_back_to_first_lines_when_no_errors() {
        let s = summarize_output("line1\nline2\nline3");
        assert!(s.contains("line1"));
    }

    #[test]
    fn first_line_truncates_long_lines() {
        let long = "x".repeat(300);
        let out = first_line(&long);
        assert!(out.chars().count() <= 158);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn render_task_block_round_trips_through_parser() {
        let block = render_task_block("deploy check", "sh -c \"echo \\\"hi\\\"\"", Some("/a/b"));
        // Appended to an existing config, the block must parse back cleanly.
        let c = parse_config(&format!("enabled = true\n{block}"));
        assert!(c.enabled);
        assert_eq!(c.task.len(), 1);
        assert_eq!(c.task[0].name, "deploy check");
        assert_eq!(c.task[0].command, "sh -c \"echo \\\"hi\\\"\"");
        assert_eq!(c.task[0].dir.as_deref(), Some("/a/b"));
    }

    #[test]
    fn toml_escape_handles_quotes_and_backslashes() {
        assert_eq!(toml_escape(r#"a"b\c"#), r#"a\"b\\c"#);
    }

    #[test]
    fn daily_guard_runs_once_per_task_per_day() {
        let dir = tempfile::tempdir().unwrap();
        let today = "2026-06-26";
        let a = DailyGuard::new(dir.path(), "task-a", today);
        let b = DailyGuard::new(dir.path(), "task-b", today);
        assert!(a.should_run());
        assert!(b.should_run());
        a.mark_done().unwrap();
        // Marking a done must not affect b (independent keys).
        assert!(!a.should_run());
        assert!(b.should_run());
    }
}
