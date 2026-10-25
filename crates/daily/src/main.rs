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
//! (`📋 daily: ran a (ok), b (fail exit 1: …)`) as `hookSpecificOutput.additionalContext` (nested; a top-level key is never delivered, GH 1343). It
//! always exits 0 (never breaks a turn).

use clap::{Parser, Subcommand};
use harness_core::append::append_line_reporting;
use harness_core::daily::DailyGuard;
use harness_core::hook::{read_stdin, run_hook, HookInput};
use serde::{Deserialize, Serialize};
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
    /// Show the results report of past daily-task runs.
    Report {
        /// Show a specific day (YYYY-MM-DD). Defaults to today.
        #[arg(long)]
        date: Option<String>,
        /// Show the last N most recent entries instead of a single day.
        #[arg(long)]
        last: Option<usize>,
    },
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
    /// Install the SessionStart hook into ~/.claude/settings.json (not yet implemented; exits 1).
    Install,
}

fn main() {
    let cli = Cli::parse();
    match cli.command {
        Cmd::SessionStart => session_start_cmd(),
        Cmd::List => list_cmd(),
        Cmd::Report { date, last } => report_cmd(date.as_deref(), last),
        Cmd::Add { name, command, dir } => add_cmd(&name, &command, dir.as_deref()),
        Cmd::Install => {
            // Nothing is installed, so this must not report success: a caller
            // scripting the install would otherwise believe the hook is wired.
            eprintln!("daily install: not yet implemented — nothing was installed; add the hook manually to ~/.claude/settings.json");
            std::process::exit(1);
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
    /// When true (default), skip the whole run if a `/flow`/`backlog` driver is
    /// actively holding the backlog lock — daily tasks then run at the next
    /// session that starts while no driver is working. Set false to always run.
    #[serde(default = "default_true")]
    skip_when_driver_active: bool,
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
            skip_when_driver_active: true,
            task: Vec::new(),
        }
    }
}

/// Parse config TOML text. Malformed TOML is an `Err` carrying the parse
/// error, NOT the default config: the default (enabled, no tasks) runs the
/// built-in security task, so substituting it would run a task the user's
/// config may have replaced or disabled (`enabled = false`). Every caller
/// refuses to act on an unparseable config.
fn parse_config(text: &str) -> Result<Config, String> {
    toml::from_str::<Config>(text).map_err(|e| e.to_string())
}

/// Load `~/.daily/config.toml`. A missing/unreadable file yields the default
/// (a read error other than NotFound is tracked separately as backlog
/// d3245d99); a file that exists but does not parse is an `Err`.
fn load_config() -> Result<Config, String> {
    match std::fs::read_to_string(config_path()) {
        Ok(text) => parse_config(&text),
        Err(_) => Ok(Config::default()),
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

        let config = match load_config() {
            Ok(c) => c,
            Err(e) => {
                // We cannot tell what the user registered (or whether daily is
                // enabled at all), so run nothing and say so — the day's task
                // status is UNKNOWN, not "nothing due".
                let msg = format!(
                    "⚠️ daily: ~/.daily/config.toml is unparseable ({}); no daily task ran — today's task status is UNKNOWN until the config is fixed",
                    first_line(&e)
                );
                eprintln!("{msg}");
                println!(
                    "{}",
                    json!({
                        "systemMessage": msg,
                        "hookSpecificOutput": {
                            "hookEventName": "SessionStart",
                            "additionalContext": msg
                        }
                    })
                );
                return;
            }
        };
        if !config.enabled {
            return;
        }

        // Don't interfere while a driver is working: if a /flow or /backlog
        // driver actively holds the backlog lock, skip silently. Tasks stay
        // "pending today" and run at the next session that starts idle. A dead
        // (stale) lock does NOT count as active, so a crashed driver won't
        // wedge daily forever.
        if config.skip_when_driver_active && driver_active() {
            return;
        }

        let now = chrono::Local::now();
        let today = now.format("%Y-%m-%d").to_string();
        let stamp = now.format("%Y-%m-%d %H:%M:%S").to_string();
        let state_dir = daily_state_dir();
        let cwd = input.cwd_or_current();

        // Run every task not yet run today, keyed independently. mark_done()
        // runs regardless of outcome — one attempt per calendar day, so a
        // FAILED task still counts as "ran today" (it won't retry until
        // tomorrow); the failure is captured in the report instead.
        let mut results: Vec<(String, Outcome)> = Vec::new();
        for task in effective_tasks(&config) {
            let guard = DailyGuard::new(&state_dir, &task.name, &today);
            if !guard.should_run() {
                continue;
            }
            let outcome = run_task(&task, &cwd);
            guard.mark_done().ok();
            append_report(&ReportEntry::of(&today, &stamp, &task.name, &outcome));
            results.push((task.name, outcome));
        }

        // Per the chosen behavior: always inject a summary of what ran today
        // (both ok and fail). Nothing ran → stay silent.
        if let Some(msg) = summary(&results) {
            println!(
                "{}",
                json!({
                    "hookSpecificOutput": {
                        "hookEventName": "SessionStart",
                        "additionalContext": msg
                    }
                })
            );
        }
    })
}

// ─────────────────────────── driver detection ──────────────────────────────

/// True if a `/flow` / `/backlog` driver is *actively* driving a queue (soft
/// dependency on the `backlog` binary). See [`driver_active_from_output`] for
/// how a failure to get an answer is resolved.
///
/// `backlog` is located with [`harness_core::plugin_bin::resolve`] (plugin
/// cache first, `$PATH` second), not by bare name: this runs as a hook, and
/// hook processes do not inherit the plugin `bin/` dirs on `$PATH` (backlog
/// abba6f0d) — a bare-name spawn read an installed-but-not-on-login-PATH
/// backlog as "not installed", i.e. "no driver".
fn driver_active() -> bool {
    use harness_core::verdict::Determination;
    let program = match harness_core::plugin_bin::resolve("backlog") {
        Determination::Known(Some(p)) => p,
        // Observed: `backlog` is not installed, so there is no queue and no
        // driver to collide with. That is an observation, not a failure to
        // observe.
        Determination::Known(None) => return driver_active_from_output(false, false, ""),
        // Could not tell (the plugin cache could not be read, or — with no
        // cache copy — a `backlog` on $PATH exists but cannot be spawned): we
        // do not know whether a driver exists. Treat as active — skip the run.
        Determination::Undetermined(why) => {
            eprintln!(
                "daily: could not locate `backlog` ({}); treating a driver as \
                 possibly active and skipping this session's run",
                why.as_str()
            );
            return driver_active_from_output(true, false, "");
        }
    };
    match Command::new(&program).args(["lock", "status"]).output() {
        Ok(out) => driver_active_from_output(
            true,
            out.status.success(),
            &String::from_utf8_lossy(&out.stdout),
        ),
        // `backlog` was located but could not be started: that is a failure to
        // get an answer, not an observation that it is absent. Same resolution
        // as "ran and failed" — active.
        Err(_) => driver_active_from_output(true, false, ""),
    }
}

/// Resolve the driver question from the outcome of shelling out to `backlog`.
/// Split out from [`driver_active`] so every arm — including the ones that
/// never produce stdout — is observable in a test.
///
/// `spawned = false` → `backlog` is absent → no queue exists → not active.
/// `spawned = true, success = false` → backlog ran and failed: we asked and did
/// not get an answer, so treat it as active (skip today's run and try again at
/// the next session) rather than trample a driver that may well be live.
fn driver_active_from_output(spawned: bool, success: bool, stdout: &str) -> bool {
    if !spawned {
        return false;
    }
    if !success {
        return true;
    }
    driver_active_from_status(stdout)
}

/// Interpret `backlog lock status` stdout: `none` → free; a JSON object with a
/// truthy `stale` field → dead holder/registration (not active); any other JSON
/// object → a live driver. Since `/flow` announces itself with a non-exclusive
/// registration rather than by taking the exclusive lock, that object may now be
/// a `kind: driver-presence` listing SEVERAL concurrent drivers, or the explicit
/// `kind: undetermined` object backlog emits when it could not read its
/// registry; both correctly read as active here.
fn driver_active_from_status(stdout: &str) -> bool {
    let trimmed = stdout.trim();
    if trimmed.is_empty() || trimmed == "none" {
        return false;
    }
    match serde_json::from_str::<serde_json::Value>(trimmed) {
        Ok(v) => !v.get("stale").and_then(|s| s.as_bool()).unwrap_or(false),
        // Unparseable but non-"none" output: be conservative and treat it as a
        // held lock (skip) rather than trampling a possibly-live driver.
        Err(_) => true,
    }
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
    /// Refused before it was ever spawned (fail-closed): blastguard flagged the
    /// command, or the task's `dir` could not be honoured.
    Blocked(String),
}

/// Resolve `task.dir` against `cwd` (the session/project root) and verify it
/// does not escape that root before it is ever handed to `Command::current_dir`.
///
/// `task.dir` comes from `~/.daily/config.toml`, a file a user edits by hand —
/// but a malformed or maliciously-crafted entry (e.g. `../../etc`) must not be
/// able to point a subprocess's working directory outside the project root.
/// We `canonicalize()` both the root and the candidate dir (resolving `..`,
/// symlinks, and relative segments) and require the candidate to be the root
/// or a descendant of it.
///
/// Any failure to honour an explicit `dir` (missing dir, canonicalize error,
/// escape, or an un-canonicalizable root) is an `Err` carrying the reason: the
/// task asked to run *somewhere specific*, and running it in the session cwd
/// instead would execute it in a place the user never chose. The caller
/// refuses the task (reported as `blocked`) rather than substituting `cwd`.
fn resolve_task_dir(task_dir: Option<&str>, cwd: &Path) -> Result<PathBuf, String> {
    let Some(raw) = task_dir else {
        // Canonicalize the default too. Every other exit from this function
        // returns a canonical path, and a return value whose meaning depends on
        // which branch produced it is a trap for callers that compare it with
        // `starts_with` against another canonical path. On macOS the difference
        // is real and routine: `/var/...` vs `/private/var/...` for the same
        // directory, which is exactly what made this fail on macos-14 while
        // passing on ubuntu. With no `dir` requested, the session cwd IS the
        // requested place, so when canonicalization is impossible the raw cwd
        // is still the right answer (no override is being dropped).
        return Ok(cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf()));
    };

    let candidate = if Path::new(raw).is_absolute() {
        PathBuf::from(raw)
    } else {
        cwd.join(raw)
    };

    let root_canon = cwd.canonicalize().map_err(|e| {
        format!(
            "task dir `{raw}`: cannot canonicalize the session cwd {} ({e})",
            cwd.display()
        )
    })?;

    match candidate.canonicalize() {
        Ok(candidate_canon) if candidate_canon.starts_with(&root_canon) => Ok(candidate_canon),
        Ok(candidate_canon) => Err(format!(
            "task dir `{raw}` resolves to {} outside the session cwd {}",
            candidate_canon.display(),
            root_canon.display()
        )),
        Err(e) => Err(format!("task dir `{raw}` is unusable ({e})")),
    }
}

/// Run one task via `sh -c`, in its `dir` (or the session cwd), with
/// `$CARGO_HOME/bin` prepended to PATH so cargo subcommands resolve even when
/// `~/.cargo/bin` isn't on the ambient PATH.
///
/// `task.command` comes from `~/.daily/config.toml`, a file a user edits by
/// hand — before it is ever handed to `sh -c`, it is run past the same pure
/// blastguard detector the PreToolUse hook uses (mirroring condukt's
/// `run_check`). A flagged command is refused fail-closed (never spawned)
/// rather than silently run.
///
/// No human is present at SessionStart to answer a blastguard `Ask` (a
/// construct it could not analyse and refuses to guess about), so the verdict
/// is [`hardened`](blastguard::model::Decision::hardened): `Ask` collapses to
/// `Deny` and the command is refused exactly like a destructive one.
fn run_task(task: &Task, cwd: &Path) -> Outcome {
    let input = serde_json::json!({ "command": task.command });
    if let blastguard::model::Decision::Deny(reason) =
        blastguard::detect::detect("Bash", Some(&input)).hardened()
    {
        return Outcome::Blocked(reason);
    }

    let dir = match resolve_task_dir(task.dir.as_deref(), cwd) {
        Ok(d) => d,
        Err(reason) => return Outcome::Blocked(reason),
    };

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
            Outcome::Blocked(reason) => format!("{name} (blocked: {})", first_line(reason)),
        })
        .collect();
    let any_fail = results.iter().any(|(_, o)| !matches!(o, Outcome::Ok));
    let icon = if any_fail { "⚠️" } else { "📋" };
    Some(format!("{icon} daily: ran {}", parts.join(", ")))
}

// ─────────────────────────── report ────────────────────────────────────────

/// One recorded task run, appended as a JSON line to `~/.daily/reports.jsonl`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct ReportEntry {
    /// Calendar day the run belongs to (YYYY-MM-DD).
    date: String,
    /// Local timestamp of the run (YYYY-MM-DD HH:MM:SS).
    at: String,
    /// Task name.
    task: String,
    /// One of "ok" | "fail" | "error".
    status: String,
    /// Process exit code (present for ok/fail; absent for spawn errors).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    code: Option<i32>,
    /// Brief detail for a failure/error (empty for ok).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    detail: String,
}

impl ReportEntry {
    fn of(date: &str, at: &str, task: &str, outcome: &Outcome) -> Self {
        let (status, code, detail) = match outcome {
            Outcome::Ok => ("ok", None, String::new()),
            Outcome::Failed { code, brief } => ("fail", *code, first_line(brief)),
            Outcome::SpawnError(e) => ("error", None, first_line(e)),
            Outcome::Blocked(reason) => ("blocked", None, first_line(reason)),
        };
        ReportEntry {
            date: date.to_string(),
            at: at.to_string(),
            task: task.to_string(),
            status: status.to_string(),
            code,
            detail,
        }
    }

    /// The status glyph used when rendering the report.
    fn glyph(&self) -> &'static str {
        match self.status.as_str() {
            "ok" => "✓",
            "fail" => "✗",
            _ => "⚠",
        }
    }
}

fn reports_path() -> PathBuf {
    daily_dir().join("reports.jsonl")
}

/// Append one entry as a JSON line. Fail-soft: any IO/serialize error is
/// swallowed (a report write must never break a turn).
fn append_report(entry: &ReportEntry) {
    let path = reports_path();
    let Ok(line) = serde_json::to_string(entry) else {
        return;
    };
    append_line_reporting(&path, &line, "daily log");
}

/// Parse the JSONL report file into entries, skipping any malformed lines.
fn parse_reports(text: &str) -> Vec<ReportEntry> {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<ReportEntry>(l).ok())
        .collect()
}

/// Render the report view. With `last = Some(n)`, show the n most recent
/// entries across all days; otherwise show every entry for `date`.
fn format_report(entries: &[ReportEntry], date: &str, last: Option<usize>) -> String {
    let mut out = String::new();
    match last {
        Some(n) => {
            let start = entries.len().saturating_sub(n);
            let recent = &entries[start..];
            if recent.is_empty() {
                return "daily report — 記録なし".to_string();
            }
            out.push_str(&format!("daily report — 直近 {} 件\n", recent.len()));
            for e in recent {
                out.push_str(&render_entry_line(e, true));
            }
        }
        None => {
            let today: Vec<&ReportEntry> = entries.iter().filter(|e| e.date == date).collect();
            if today.is_empty() {
                return format!("daily report — {date}: 記録なし");
            }
            out.push_str(&format!("daily report — {date}\n"));
            for e in today {
                out.push_str(&render_entry_line(e, false));
            }
        }
    }
    out.trim_end().to_string()
}

/// One report line. `with_date` prefixes the day (for the cross-day --last view).
fn render_entry_line(e: &ReportEntry, with_date: bool) -> String {
    let status = match (e.status.as_str(), e.code) {
        ("ok", _) => "ok".to_string(),
        (_, Some(c)) => format!("{} exit {c}", e.status),
        (_, None) => e.status.clone(),
    };
    let detail = if e.detail.is_empty() {
        String::new()
    } else {
        format!(": {}", e.detail)
    };
    let prefix = if with_date {
        format!("{} {}", e.date, e.at.split(' ').nth(1).unwrap_or(&e.at))
    } else {
        e.at.split(' ').nth(1).unwrap_or(&e.at).to_string()
    };
    format!(
        "  {} {:<16} [{}] {}{}\n",
        e.glyph(),
        e.task,
        prefix,
        status,
        detail
    )
}

fn report_cmd(date: Option<&str>, last: Option<usize>) -> ! {
    let text = std::fs::read_to_string(reports_path()).unwrap_or_default();
    let entries = parse_reports(&text);
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let date = date.unwrap_or(&today);
    println!("{}", format_report(&entries, date, last));
    std::process::exit(0);
}

// ─────────────────────────── list / add ────────────────────────────────────

fn list_cmd() -> ! {
    let config = match load_config() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("daily list: ~/.daily/config.toml is unparseable ({e}); registered tasks are UNKNOWN");
            std::process::exit(1);
        }
    };
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
    // An unparseable config means the duplicate check cannot be performed, and
    // appending to it would leave it just as broken: refuse.
    let config = match parse_config(&existing) {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "daily add: {} is unparseable ({e}); fix it before registering a task",
                path.display()
            );
            std::process::exit(1);
        }
    };
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
        let c = parse_config("enabled = false\n").unwrap();
        assert!(!c.enabled);
    }

    #[test]
    fn parse_config_missing_enabled_defaults_true() {
        // A config that only registers a task is still enabled.
        let c = parse_config("[[task]]\nname = \"x\"\ncommand = \"echo hi\"\n").unwrap();
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
        let c = parse_config(text).unwrap();
        assert_eq!(c.task.len(), 2);
        assert_eq!(c.task[1].dir.as_deref(), Some("/repo"));
    }

    /// Contract change (backlog f1f06417): malformed TOML used to fall back to
    /// `Config::default()`, which runs the built-in security task. It is now an
    /// `Err`, and every caller refuses to act on it.
    #[test]
    fn parse_config_malformed_is_an_error_not_the_default() {
        let c = parse_config("this is not = valid toml ][");
        assert!(c.is_err(), "malformed config must not parse: {c:?}");
        assert!(!c.unwrap_err().is_empty(), "the error carries a reason");
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
        let c = parse_config("[[task]]\nname = \"only\"\ncommand = \"true\"\n").unwrap();
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
        let c = parse_config(&format!("enabled = true\n{block}")).unwrap();
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
    fn config_skip_when_driver_active_defaults_true() {
        assert!(Config::default().skip_when_driver_active);
        // Omitted in config → still defaults true.
        let c = parse_config("[[task]]\nname = \"x\"\ncommand = \"true\"\n").unwrap();
        assert!(c.skip_when_driver_active);
        // Explicitly disabled.
        let c = parse_config("skip_when_driver_active = false\n").unwrap();
        assert!(!c.skip_when_driver_active);
    }

    #[test]
    fn driver_active_reads_backlog_lock_status() {
        // Free lock.
        assert!(!driver_active_from_status("none"));
        assert!(!driver_active_from_status("  none\n"));
        assert!(!driver_active_from_status(""));
        // A live (active) holder → JSON without a stale marker.
        assert!(driver_active_from_status(
            r#"{"pid":123,"session_id":"s","project":"/p"}"#
        ));
        // A dead (stale) holder → not active, daily may run.
        assert!(!driver_active_from_status(
            r#"{"pid":123,"session_id":"s","stale":true}"#
        ));
        // stale:false is still active.
        assert!(driver_active_from_status(r#"{"pid":1,"stale":false}"#));
        // Unparseable non-"none" → conservatively treated as held.
        assert!(driver_active_from_status("garbage-but-not-none"));
    }

    /// New contract: `/flow` registers non-exclusive presence instead of taking
    /// the exclusive lock, so liveness arrives as a `driver-presence` object
    /// that may list SEVERAL concurrent drivers. daily must still read that as
    /// "a driver is active" and skip.
    #[test]
    fn driver_active_reads_the_non_exclusive_driver_presence_shape() {
        assert!(driver_active_from_status(
            r#"{"kind":"driver-presence","session_id":"a","pid":1,"project":"/p","acquired_at":0,"heartbeat_at":9,"driver_count":1,"drivers":[{"session_id":"a"}]}"#
        ));
        assert!(driver_active_from_status(
            r#"{"kind":"driver-presence","session_id":"b","pid":1,"project":"/p","acquired_at":0,"heartbeat_at":9,"driver_count":2,"drivers":[{"session_id":"a"},{"session_id":"b"}]}"#
        ));
        // Only stale registrations remain -> daily may run.
        assert!(!driver_active_from_status(
            r#"{"kind":"driver-presence","session_id":"ghost","stale":true,"driver_count":0,"drivers":[]}"#
        ));
        // backlog could not read its registry and says so -> not an "all clear".
        assert!(driver_active_from_status(
            r#"{"kind":"undetermined","undetermined":true,"reason":"registry unreadable"}"#
        ));
    }

    /// The arms that produce no stdout at all. Asking and not getting an answer
    /// is not the same as being told the queue is idle.
    #[test]
    fn driver_active_from_output_resolves_a_failed_query_to_active() {
        assert!(
            !driver_active_from_output(false, false, ""),
            "backlog not installed -> no queue exists -> daily may run"
        );
        assert!(
            driver_active_from_output(true, false, ""),
            "backlog ran and failed -> we have no answer -> skip rather than trample a driver"
        );
        assert!(!driver_active_from_output(true, true, "none"));
        assert!(driver_active_from_output(
            true,
            true,
            r#"{"kind":"driver-presence","driver_count":1,"drivers":[{"session_id":"a"}]}"#
        ));
    }

    #[test]
    fn report_entry_captures_each_outcome() {
        let ok = ReportEntry::of("2026-07-03", "2026-07-03 10:00:00", "sec", &Outcome::Ok);
        assert_eq!(ok.status, "ok");
        assert!(ok.detail.is_empty());
        assert_eq!(ok.glyph(), "✓");

        let fail = ReportEntry::of(
            "2026-07-03",
            "2026-07-03 10:00:00",
            "sec",
            &Outcome::Failed {
                code: Some(2),
                brief: "error: boom\nmore".to_string(),
            },
        );
        assert_eq!(fail.status, "fail");
        assert_eq!(fail.code, Some(2));
        assert_eq!(fail.detail, "error: boom");
        assert_eq!(fail.glyph(), "✗");

        let err = ReportEntry::of(
            "2026-07-03",
            "2026-07-03 10:00:00",
            "sec",
            &Outcome::SpawnError("No such file".to_string()),
        );
        assert_eq!(err.status, "error");
        assert_eq!(err.code, None);
    }

    #[test]
    fn report_jsonl_round_trips() {
        let e = ReportEntry::of("2026-07-03", "2026-07-03 10:00:00", "sec", &Outcome::Ok);
        let line = serde_json::to_string(&e).unwrap();
        let parsed = parse_reports(&format!("{line}\n\nnot json\n"));
        assert_eq!(parsed.len(), 1, "malformed lines are skipped");
        assert_eq!(parsed[0], e);
    }

    #[test]
    fn format_report_by_day_filters_to_date() {
        let entries = vec![
            ReportEntry::of("2026-07-02", "2026-07-02 09:00:00", "a", &Outcome::Ok),
            ReportEntry::of(
                "2026-07-03",
                "2026-07-03 10:00:00",
                "b",
                &Outcome::Failed {
                    code: Some(1),
                    brief: "nope".to_string(),
                },
            ),
        ];
        let out = format_report(&entries, "2026-07-03", None);
        assert!(out.contains("2026-07-03"));
        assert!(out.contains("b"));
        assert!(out.contains("fail exit 1"));
        assert!(!out.contains(" a "), "other day's task excluded: {out}");

        // An empty day reports "記録なし".
        let empty = format_report(&entries, "2026-07-01", None);
        assert!(empty.contains("記録なし"));
    }

    #[test]
    fn format_report_last_n_shows_recent_across_days() {
        let entries: Vec<ReportEntry> = (0..5)
            .map(|i| {
                ReportEntry::of(
                    "2026-07-03",
                    &format!("2026-07-03 10:0{i}:00"),
                    &format!("t{i}"),
                    &Outcome::Ok,
                )
            })
            .collect();
        let out = format_report(&entries, "2026-07-03", Some(2));
        assert!(out.contains("直近 2 件"));
        assert!(out.contains("t4"));
        assert!(out.contains("t3"));
        assert!(!out.contains("t0"));
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

    #[test]
    fn resolve_task_dir_defaults_to_cwd_when_unset() {
        let root = tempfile::tempdir().unwrap();
        let resolved = resolve_task_dir(None, root.path());
        assert_eq!(resolved, Ok(root.path().canonicalize().unwrap()));
    }

    #[test]
    fn resolve_task_dir_accepts_subdir_within_root() {
        let root = tempfile::tempdir().unwrap();
        let sub = root.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        let resolved = resolve_task_dir(Some("sub"), root.path());
        assert_eq!(resolved, Ok(sub.canonicalize().unwrap()));
    }

    // Contract change (backlog 24a169f4): the three rejection cases below used
    // to pin a silent fallback to the session cwd. A rejected `dir` is now an
    // `Err`, so the task is refused instead of running somewhere unrequested.

    #[test]
    fn resolve_task_dir_rejects_parent_traversal_escape() {
        let root = tempfile::tempdir().unwrap();
        // e.g. `../../etc` — escapes the project root via `..` traversal.
        let resolved = resolve_task_dir(Some("../../etc"), root.path());
        assert!(resolved.is_err(), "escape must be rejected: {resolved:?}");
        // The refusal names the offending dir, so the blocked report says why.
        let err = resolved.unwrap_err();
        assert!(err.contains("../../etc"), "got: {err}");
    }

    #[test]
    fn resolve_task_dir_rejects_absolute_escape_outside_root() {
        let root = tempfile::tempdir().unwrap();
        // An absolute path outside the root must also be rejected.
        let resolved = resolve_task_dir(Some("/etc"), root.path());
        let err = resolved.expect_err("absolute escape must be rejected");
        assert!(err.contains("outside the session cwd"), "got: {err}");
    }

    #[test]
    fn resolve_task_dir_rejects_nonexistent_dir() {
        let root = tempfile::tempdir().unwrap();
        let resolved = resolve_task_dir(Some("does-not-exist"), root.path());
        let err = resolved.expect_err("missing dir must be rejected");
        assert!(err.contains("does-not-exist"), "got: {err}");
    }

    /// A task whose `dir` cannot be honoured is refused, never spawned.
    #[test]
    fn run_task_blocks_when_dir_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let task = Task {
            name: "nodir".to_string(),
            command: "touch spawned.marker".to_string(),
            dir: Some("no-such-subdir".to_string()),
        };
        let outcome = run_task(&task, tmp.path());
        assert!(
            matches!(outcome, Outcome::Blocked(_)),
            "expected Blocked, got {outcome:?}"
        );
        assert!(!tmp.path().join("spawned.marker").exists());
    }

    /// A destructive task.command (rm -rf) must be refused by blastguard before
    /// spawn: no process is created, so the sentinel file never gets touched,
    /// and run_task reports Blocked instead of running the command.
    #[test]
    fn run_task_blocks_destructive_command_via_blastguard() {
        let tmp = tempfile::tempdir().unwrap();
        let sentinel = tmp.path().join("ran.txt");
        let task = Task {
            name: "destructive".to_string(),
            command: format!("touch {} && rm -rf /nonexistent", sentinel.display()),
            dir: None,
        };
        let outcome = run_task(&task, tmp.path());
        assert!(
            matches!(outcome, Outcome::Blocked(_)),
            "expected Blocked, got {outcome:?}"
        );
        assert!(
            !sentinel.exists(),
            "command must never spawn — sentinel file should not exist"
        );
    }

    /// A benign task.command is unaffected by the blastguard gate and runs
    /// normally through to completion.
    #[test]
    fn run_task_allows_benign_command() {
        let tmp = tempfile::tempdir().unwrap();
        let task = Task {
            name: "benign".to_string(),
            command: "true".to_string(),
            dir: None,
        };
        let outcome = run_task(&task, tmp.path());
        assert!(
            matches!(outcome, Outcome::Ok),
            "expected Ok, got {outcome:?}"
        );
    }
}
