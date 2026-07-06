//! stuckguard — stuck-loop detector + escalation for Claude Code.
//!
//! One binary, one subcommand per job. `watch` is the **PostToolUse** hook: it
//! records each tool call into a per-session window and, when it spots a stuck
//! pattern (the same action on repeat, or edit thrash), injects a nudge that
//! grows into "stop and ask the user". It can only *advise* — never block a tool
//! call or end a turn — so a false positive costs at most one extra line.

mod config;
mod detect;
mod install;
mod model;
mod sig;
mod state;

use std::path::Path;

use clap::{Parser, Subcommand};
use serde_json::json;

use harness_core::hook::{read_stdin, run_hook};

use config::Config;
use detect::{Kind, Trip};
use model::HookInput;

#[derive(Parser)]
#[command(
    name = "stuckguard",
    version,
    about = "Stuck-loop detector + escalation for Claude Code (PostToolUse hook)."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// PostToolUse hook: record the call, detect stuck patterns, nudge.
    Watch,
    /// Merge the stuckguard PostToolUse hook into ~/.claude/settings.json.
    Install {
        #[arg(long)]
        dry_run: bool,
    },
    /// Remove the stuckguard hook from ~/.claude/settings.json.
    Uninstall {
        #[arg(long)]
        dry_run: bool,
    },
    /// Write a starter ./stuckguard.toml.
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
        Command::Watch => run_hook(watch),
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

fn watch() {
    if Config::disabled_env() {
        return;
    }
    let raw = read_stdin();
    let Some(input) = HookInput::parse(&raw) else {
        return;
    };
    let root = input.cwd_or_current();
    let cfg = Config::load(&root);
    if !cfg.enabled || cfg.is_ignored(&input.tool_name) {
        return;
    }
    let Some(event) = sig::build(
        &input.tool_name,
        input.tool_input.as_ref(),
        input.tool_response.as_ref(),
    ) else {
        return;
    };

    let session = input.session_key();
    let mut st = state::load(&cfg.state_dir, &session);
    let seq = st.push(event, cfg.window);

    let trip = detect::detect(&st.events, &cfg);
    let mut emitted = None;

    if let Some(t) = &trip {
        if !st.in_cooldown(&t.key, seq, cfg.cooldown_events) {
            let count = st.record_nudge(&t.key, seq);
            let escalated = count >= cfg.escalate_after;
            log_event(&cfg, &session, t, count, escalated);
            emitted = Some(message(t, escalated, count));
        }
    }

    state::save(&cfg.state_dir, &session, &st);

    if let Some(text) = emitted {
        let out = json!({
            "hookSpecificOutput": {
                "hookEventName": "PostToolUse",
                "additionalContext": text,
            }
        });
        println!("{out}");
    }
}

/// The escalating advice injected back into the conversation.
fn message(t: &Trip, escalated: bool, count: u32) -> String {
    let head = match t.kind {
        Kind::Repeat => {
            let fail = if t.all_errored {
                "（毎回失敗しています）"
            } else {
                ""
            };
            format!("🔁 stuckguard: 同じ操作の繰り返しを検知 — {}{fail}。", t.detail)
        }
        Kind::Oscillation => format!(
            "↔ stuckguard: ファイル `{}` で編集の打ち消し合い（revert thrash）を {} 回検知。同じ箇所を行き来しています。",
            t.detail, t.count
        ),
    };

    if escalated {
        format!(
            "{head}\n🛑 同じ試行を繰り返さないでください（通知 {count} 回目）。いったんツール呼び出しを止め、\
             (1) 何を試して何が失敗したか (2) 現在の仮説 (3) 判断に必要な情報 を簡潔に整理し、\
             **ユーザーに状況を報告して指示を仰いで**ください。"
        )
    } else {
        format!(
            "{head}\n一歩引いて前提を疑い、別アプローチを検討してください。根本原因が不明なまま同じ操作を繰り返さないこと。\
             抜け出せない場合はユーザーに状況を共有して指示を仰いでください。"
        )
    }
}

fn log_event(cfg: &Config, session: &str, t: &Trip, count: u32, escalated: bool) {
    let path = cfg.state_dir.join("log.jsonl");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let kind = match t.kind {
        Kind::Repeat => "repeat",
        Kind::Oscillation => "oscillation",
    };
    let entry = json!({
        "ts": chrono::Local::now().to_rfc3339(),
        "session": session,
        "kind": kind,
        "key": t.key,
        "count": t.count,
        "nudge": count,
        "escalated": escalated,
        "detail": t.detail,
    });
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    if let (Ok(line), Ok(mut f)) = (serde_json::to_string(&entry), opts.open(&path)) {
        use std::io::Write;
        let _ = writeln!(f, "{line}");
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
    println!("config:                {}", src.display());
    println!("enabled:               {}", cfg.enabled);
    println!("window:                {}", cfg.window);
    println!("repeat_threshold:      {}", cfg.repeat_threshold);
    println!("oscillation_threshold: {}", cfg.oscillation_threshold);
    println!("cooldown_events:       {}", cfg.cooldown_events);
    println!("escalate_after:        {}", cfg.escalate_after);
    println!("ignore_tools:          {}", cfg.ignore_tools.join(", "));
    println!("state_dir:             {}", cfg.state_dir.display());
}

const STARTER: &str = r#"# stuckguard.toml — stuck-loop detector + escalation for Claude Code.
#
# A PostToolUse hook records each tool call into a per-session window and nudges
# the agent when it detects a stuck pattern. It only injects advice — it can
# never block a tool call or end a turn.

enabled = true
window = 12                 # recent tool events kept per session and inspected
repeat_threshold = 3        # same normalized (tool, input) N times -> nudge
oscillation_threshold = 2   # edit revert-thrash reversals on one file -> nudge
cooldown_events = 6         # don't re-nudge the same pattern within N events
escalate_after = 2          # after N nudges for a pattern, escalate to "ask the user"
ignore_tools = ["TodoWrite"]
# state_dir = "~/.stuckguard/state"
"#;

fn init(force: bool) -> anyhow::Result<()> {
    let root = std::env::current_dir()?;
    std::fs::create_dir_all(Config::default().state_dir)?;
    let path = Config::project_path(&root);
    if path.exists() && !force {
        anyhow::bail!(
            "{} already exists (use --force to overwrite)",
            path.display()
        );
    }
    std::fs::write(&path, STARTER)?;
    println!("wrote {}", path.display());
    println!("Run `stuckguard install` once to wire the PostToolUse hook.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn log_event_creates_file_with_0o600() {
        use std::os::unix::fs::PermissionsExt;

        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "stuckguard-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();

        let cfg = Config {
            state_dir: dir.clone(),
            ..Config::default()
        };
        let trip = Trip {
            key: "repeat:test".to_string(),
            kind: Kind::Repeat,
            count: 3,
            all_errored: false,
            detail: "test detail".to_string(),
        };

        log_event(&cfg, "sess", &trip, 1, false);

        let log_path = dir.join("log.jsonl");
        let meta = std::fs::metadata(&log_path).expect("log file should exist");
        let mode = meta.permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "expected mode 0o600, got {mode:o}");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
