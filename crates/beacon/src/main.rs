//! beacon — desktop & webhook notifications for Claude Code.
//!
//! Two hooks, one job: tell you when to look back at the terminal. `notify` is
//! the **Stop** and **Notification** hook — on Stop it pings "turn finished",
//! on Notification it relays Claude's "needs your input/permission" message —
//! through whatever channels you configured (desktop, Slack, webhook, command).
//!
//! Like the rest of the toolkit it can only *notify*: the hook never blocks a
//! turn and always exits 0, so a missing `curl`, denied notification, or empty
//! stdin costs nothing.

mod config;
mod install;
mod model;
mod notify;
mod transcript;

use std::path::Path;

use clap::{Parser, Subcommand};
use serde_json::json;

use harness_core::hook::{read_stdin, run_hook};

use config::Config;
use model::HookInput;
use notify::Note;

#[derive(Parser)]
#[command(
    name = "beacon",
    version,
    about = "Desktop & webhook notifications for Claude Code (Stop + Notification hooks)."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Stop / Notification hook: deliver a notification through every channel.
    Notify,
    /// Send a sample notification through the configured channels (setup check).
    Test,
    /// Merge the beacon Stop + Notification hooks into ~/.claude/settings.json.
    Install {
        #[arg(long)]
        dry_run: bool,
    },
    /// Remove the beacon hooks from ~/.claude/settings.json.
    Uninstall {
        #[arg(long)]
        dry_run: bool,
    },
    /// Write a starter ./beacon.toml.
    Init {
        #[arg(long)]
        force: bool,
    },
    /// Show the resolved config and active channels.
    Status,
    /// Trust the current project so its beacon.toml `command` is honored.
    Trust,
}

fn main() {
    let cli = Cli::parse();
    match cli.command {
        Command::Notify => run_hook(notify_hook),
        Command::Test => test(),
        Command::Install { dry_run } => exit_on_err(install::install(dry_run)),
        Command::Uninstall { dry_run } => exit_on_err(install::uninstall(dry_run)),
        Command::Init { force } => exit_on_err(init(force)),
        Command::Status => status(),
        Command::Trust => exit_on_err(trust_project()),
    }
}

/// Trust the current project root so a project-local `beacon.toml` may run its
/// escape-hatch `command`. Mirrors VS Code Workspace Trust / git safe.directory.
fn trust_project() -> anyhow::Result<()> {
    let root = std::env::current_dir()?;
    let key = harness_core::trust::add(&root)?;
    println!("trusted {}", key.display());
    println!("beacon.toml `command` from this project will now be honored.");
    Ok(())
}

fn exit_on_err(r: anyhow::Result<()>) {
    if let Err(e) = r {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

/// Build the notification for an event, or None if this event is muted.
fn build_note(cfg: &Config, input: &HookInput) -> Option<Note> {
    let project = input.project_name();
    match input.hook_event_name.as_str() {
        "Stop" => {
            if !cfg.on_stop {
                return None;
            }
            let body = cfg
                .include_snippet
                .then(|| transcript::last_assistant_text(&input.transcript_path, cfg.snippet_chars))
                .flatten()
                .unwrap_or_else(|| "ターンが完了しました。".to_string());
            Some(Note {
                event: "stop",
                title: format!("✅ {project} — 完了"),
                body,
                project,
            })
        }
        "Notification" => {
            if !cfg.on_notification {
                return None;
            }
            let body = if input.message.trim().is_empty() {
                "入力待ちです。".to_string()
            } else {
                input.message.clone()
            };
            Some(Note {
                event: "attention",
                title: format!("🔔 {project} — 確認"),
                body,
                project,
            })
        }
        _ => None,
    }
}

fn notify_hook() {
    if Config::disabled_env() {
        return;
    }
    let raw = read_stdin();
    let Some(input) = HookInput::parse(&raw) else {
        return;
    };
    let root = input.cwd_or_current();
    let cfg = Config::load(&root);
    if !cfg.enabled || !cfg.any_channel() {
        return;
    }
    let Some(note) = build_note(&cfg, &input) else {
        return;
    };
    let sent = notify::dispatch(&cfg, &note);
    log_event(&cfg, &note, &sent);
}

fn log_event(cfg: &Config, note: &Note, sent: &[&str]) {
    if !cfg.log {
        return;
    }
    let path = cfg.state_dir.join("log.jsonl");
    let entry = json!({
        "ts": chrono::Local::now().to_rfc3339(),
        "event": note.event,
        "project": note.project,
        "title": note.title,
        "channels": sent,
    });
    if let Ok(line) = serde_json::to_string(&entry) {
        // Use harness_core::append::append_line to write as a single atomic
        // write() syscall, preventing concurrent O_APPEND interleaving under
        // parallel beacon notifications. See crates/harness-core/src/append.rs.
        harness_core::append::append_line(&path, &line);
    }
}

fn test() {
    let root = std::env::current_dir().unwrap_or_else(|_| Path::new(".").to_path_buf());
    let cfg = Config::load(&root);
    let project = root
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("project")
        .to_string();
    if !cfg.any_channel() {
        println!("no channels configured. Enable `desktop`, set `slack_webhook`/`webhook`, or a `command` in beacon.toml.");
        return;
    }
    let note = Note {
        event: "test",
        title: format!("🔔 {project} — beacon test"),
        body: "beacon の通知テストです。これが見えていれば設定OK。".to_string(),
        project,
    };
    let sent = notify::dispatch(&cfg, &note);
    if sent.is_empty() {
        println!("no channel delivered (is `curl`/`osascript`/`notify-send` available?).");
    } else {
        println!("sent via: {}", sent.join(", "));
    }
}

fn status() {
    let root = std::env::current_dir().unwrap_or_else(|_| Path::new(".").to_path_buf());
    let cfg = Config::load(&root);
    let src = if Config::project_path(&root).exists() {
        Config::project_path(&root)
    } else if Config::home_path().exists() {
        Config::home_path()
    } else {
        Path::new("(defaults — no config file)").to_path_buf()
    };
    let mask = |o: &Option<String>| match o {
        Some(_) => "set",
        None => "-",
    };
    println!("config:           {}", src.display());
    println!("enabled:          {}", cfg.enabled);
    println!("on_stop:          {}", cfg.on_stop);
    println!("on_notification:  {}", cfg.on_notification);
    println!(
        "include_snippet:  {} ({} chars)",
        cfg.include_snippet, cfg.snippet_chars
    );
    println!(
        "desktop:          {} (os: {})",
        cfg.desktop,
        std::env::consts::OS
    );
    println!("sound:            {}", cfg.sound.as_deref().unwrap_or("-"));
    println!("slack_webhook:    {}", mask(&cfg.slack_webhook));
    println!("webhook:          {}", mask(&cfg.webhook));
    println!(
        "command:          {}",
        cfg.command.as_deref().unwrap_or("-")
    );
    println!("log:              {}", cfg.log);
    println!("state_dir:        {}", cfg.state_dir.display());
    println!("\nrun `beacon test` to fire a sample notification.");
}

const STARTER: &str = r#"# beacon.toml — desktop & webhook notifications for Claude Code.
#
# Stop and Notification hooks ping you when a turn finishes or Claude needs your
# input, so you can step away from a long session. The hook only notifies — it
# never blocks a turn. Set BEACON_DISABLE=1 to silence everything.

enabled = true
on_stop = true            # notify when a turn finishes (Stop)
on_notification = true    # notify when Claude needs input/permission (Notification)
include_snippet = true    # on Stop, append a tail of Claude's last message
snippet_chars = 160

# --- channels (any combination) ---
desktop = true            # macOS osascript / Linux notify-send
# sound = "Glass"         # macOS notification sound name (omit = silent)

# Slack incoming webhook. Prefer the env var BEACON_SLACK_WEBHOOK so the URL
# isn't committed; it overrides this field when set.
# slack_webhook = "https://hooks.slack.com/services/XXX/YYY/ZZZ"

# Generic webhook — receives {event, project, title, body} as JSON POST.
# webhook = "https://example.com/hook"

# Escape hatch: a shell command run with BEACON_EVENT / BEACON_PROJECT /
# BEACON_TITLE / BEACON_BODY in the environment.
# command = "terminal-notifier -title \"$BEACON_TITLE\" -message \"$BEACON_BODY\""

log = true                # append deliveries to <state_dir>/log.jsonl
# state_dir = "~/.beacon/state"
"#;

fn init(force: bool) -> anyhow::Result<()> {
    let root = std::env::current_dir()?;
    let path = Config::project_path(&root);
    if path.exists() && !force {
        anyhow::bail!(
            "{} already exists (use --force to overwrite)",
            path.display()
        );
    }
    std::fs::write(&path, STARTER)?;
    println!("wrote {}", path.display());
    println!("Run `beacon test` to check delivery, then `beacon install` to wire the hooks.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression test for concurrent log_event appends. With the old writeln!
    /// pattern (body + '\n' as two syscalls), concurrent O_APPEND writes could
    /// interleave, concatenating two JSONL records onto one line. With
    /// append_line (single write() syscall), all records must survive as
    /// individually parseable JSON objects.
    #[test]
    fn concurrent_log_events_never_interleave_records() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log_path = dir.path().join("log.jsonl");

        const THREADS: usize = 8;
        const PER_THREAD: usize = 100;

        // Simulate concurrent beacon notifications by spawning threads
        // that call log_event in parallel with varying payload sizes.
        std::thread::scope(|scope| {
            for t in 0..THREADS {
                let log_path = log_path.clone();
                scope.spawn(move || {
                    let cfg = Config {
                        enabled: true,
                        on_stop: true,
                        on_notification: true,
                        include_snippet: false,
                        snippet_chars: 160,
                        desktop: false,
                        sound: None,
                        slack_webhook: None,
                        webhook: None,
                        command: None,
                        log: true,
                        state_dir: log_path.parent().unwrap().to_path_buf(),
                    };
                    for i in 0..PER_THREAD {
                        let note = Note {
                            event: "test",
                            title: format!("test {t}-{i}"),
                            body: "x".repeat(i % 64),
                            project: format!("proj-{t}"),
                        };
                        let sent = vec!["ch1"];
                        log_event(&cfg, &note, &sent);
                    }
                });
            }
        });

        // Verify every line is a complete JSON object (no interleaving).
        let text = std::fs::read_to_string(&log_path).expect("read log");
        let mut count = 0usize;
        for (n, l) in text.lines().enumerate() {
            if l.is_empty() {
                continue;
            }
            serde_json::from_str::<serde_json::Value>(l)
                .unwrap_or_else(|e| panic!("line {n} is not valid JSON: {e} — {l:.120}"));
            count += 1;
        }
        assert_eq!(
            count,
            THREADS * PER_THREAD,
            "all {count} appended records must survive as individually parseable JSON lines"
        );
    }
}
