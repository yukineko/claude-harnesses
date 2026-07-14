mod config;
mod hooks;
mod install;
mod lock;
mod store;
mod task;

use anyhow::Result;
use clap::{Parser, Subcommand};
use harness_core::hook::{read_stdin, run_hook, HookInput};
use serde_json::json;

#[derive(Parser)]
#[command(name = "backlog", about = "Cross-project task queue for Claude Code")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Add a new task to the backlog
    Add {
        /// Task title
        #[arg(long)]
        title: String,

        /// Project path
        #[arg(long)]
        project: String,

        /// Tags (can be specified multiple times)
        #[arg(long = "tag", action = clap::ArgAction::Append)]
        tags: Vec<String>,

        /// Priority shortcut: p0, p1, or p2 (added as a tag)
        #[arg(long)]
        priority: Option<String>,

        /// Notes
        #[arg(long, default_value = "")]
        notes: String,

        /// Ordering weight (higher surfaces first within a priority tier).
        /// Supply a compass opportunity's weight here so the queue order tracks
        /// opportunity impact, not just priority + insertion time. Default 0.0
        /// preserves the legacy (priority, created_at) order.
        #[arg(long, default_value_t = 0.0)]
        weight: f64,

        /// Bypass the duplicate-content guard: add even when an existing
        /// pending/failed task, or a live cross-session claim, already holds
        /// this title+project's content hashkey.
        #[arg(long)]
        force: bool,
    },

    /// List tasks
    List {
        /// Filter by tag
        #[arg(long)]
        tag: Option<String>,

        /// Filter by project path
        #[arg(long)]
        project: Option<String>,

        /// Filter by status: pending | done | failed (NB: not "open" — that is
        /// hypothesis's vocabulary, a different binary)
        #[arg(long)]
        status: Option<String>,

        /// Emit the matching tasks as a JSON array (for machine consumers like
        /// autoflow) instead of the human table.
        #[arg(long)]
        json: bool,
    },

    /// Show the next highest-priority pending task
    Next {
        /// Filter by tag
        #[arg(long)]
        tag: Option<String>,

        /// Filter by project path
        #[arg(long)]
        project: Option<String>,

        /// Atomically reserve the returned task (CA-backlog-001): marks it
        /// `claimed` under the tasks-file lock in the same critical section
        /// that selects it, so two concurrent `next --claim` callers cannot
        /// both be handed the same pending task. Without this flag, `next`
        /// keeps its pre-existing pure-read behavior (no lock, no mutation).
        #[arg(long)]
        claim: bool,
    },

    /// Mark a task as done
    Done {
        /// Task ID
        id: String,
    },

    /// Mark a task as failed
    Fail {
        /// Task ID
        id: String,

        /// Failure reason
        #[arg(long)]
        reason: Option<String>,
    },

    /// Edit a task's fields
    Edit {
        /// Task ID
        id: String,

        /// New title
        #[arg(long)]
        title: Option<String>,

        /// New tags (replaces existing tags)
        #[arg(long = "tag", action = clap::ArgAction::Append)]
        tags: Vec<String>,

        /// New notes
        #[arg(long)]
        notes: Option<String>,

        /// New status
        #[arg(long)]
        status: Option<String>,
    },

    /// SessionStart hook: reads stdin JSON and injects pending tasks as context
    SessionStart,

    /// Install hooks into ~/.claude/settings.json
    Install {
        #[arg(long)]
        dry_run: bool,
    },

    /// Remove hooks from ~/.claude/settings.json
    Uninstall {
        #[arg(long)]
        dry_run: bool,
    },

    /// Manage the ~/.backlog/run.lock exclusive lock
    Lock {
        #[command(subcommand)]
        action: LockAction,
    },
}

#[derive(Subcommand)]
enum LockAction {
    /// Acquire the lock (errors if already active)
    Acquire {
        /// Session ID
        #[arg(long)]
        session_id: String,

        /// Project path
        #[arg(long)]
        project: String,

        /// Steal the lock even from a live holder (強制奪取). Without this, a
        /// dead holder's lock is reaped automatically but a live one errors.
        #[arg(long)]
        force: bool,
    },

    /// Release the lock (no-op if none)
    Release,

    /// Print lock status as JSON, or "none"
    Status,
}

fn main() {
    let cli = Cli::parse();
    if let Err(e) = run(cli) {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<()> {
    let cfg = config::Config::load();
    let tasks_path = cfg.tasks_path();

    match cli.command {
        Command::Add {
            title,
            project,
            mut tags,
            priority,
            notes,
            weight,
            force,
        } => {
            // priority is a shortcut for adding a priority tag
            if let Some(p) = priority {
                if !tags.contains(&p) {
                    tags.push(p);
                }
            }
            let now = now_unix();
            let id = store::add_with_weight(
                &tasks_path,
                &title,
                &project,
                tags,
                &notes,
                weight,
                force,
                now,
            )?;
            println!("added: {id}");
        }

        Command::List {
            tag,
            project,
            status,
            json: as_json,
        } => {
            // A typo'd status used to silently match nothing ("no tasks"),
            // indistinguishable from a genuinely empty queue. Warn loudly so an
            // unknown filter value (e.g. the wrong `open`) is obvious. The check
            // lives in `task::status_warning` so it is unit-tested.
            if let Some(w) = task::status_warning(status.as_deref()) {
                eprintln!("{w}");
            }
            let tasks = store::list(
                &tasks_path,
                tag.as_deref(),
                project.as_deref(),
                status.as_deref(),
            )?;

            if as_json {
                // Machine-readable array (consumed by autoflow). Each task keeps
                // its real field names (notably `title` and `status`), so callers
                // deserialize a subset and ignore the rest. `hashkey` is computed
                // (not stored) so callers like `/flow` can gate on
                // `condukt state is-claimed --hashkey <h>` without recomputing the
                // normalization themselves.
                let with_hashkey: Vec<serde_json::Value> = tasks
                    .iter()
                    .map(|t| {
                        let mut v = serde_json::to_value(t).unwrap_or_default();
                        if let Some(obj) = v.as_object_mut() {
                            obj.insert(
                                "hashkey".to_string(),
                                serde_json::Value::String(task::hashkey(&t.title, &t.project)),
                            );
                        }
                        v
                    })
                    .collect();
                println!("{}", serde_json::to_string(&with_hashkey)?);
            } else if tasks.is_empty() {
                println!("no tasks");
            } else {
                let now = now_unix();
                println!("{:<10} {:<10} {:<10} TITLE", "ID", "PRIORITY", "STATUS");
                for t in &tasks {
                    let priority_str = match t.priority() {
                        0 => "p0",
                        1 => "p1",
                        2 => "p2",
                        _ => "-",
                    };
                    let status_str = if t.is_deferred(now) {
                        "deferred".to_string()
                    } else {
                        t.status.clone()
                    };
                    println!(
                        "{:<10} {:<10} {:<10} {}",
                        t.id, priority_str, status_str, t.title
                    );
                }
            }
        }

        Command::Next {
            tag,
            project,
            claim,
        } => {
            let task = if claim {
                store::next_claim(&tasks_path, tag.as_deref(), project.as_deref())?
            } else {
                store::next(&tasks_path, tag.as_deref(), project.as_deref())?
            };
            match task {
                Some(t) => {
                    println!("{}", serde_json::to_string_pretty(&t)?);
                }
                None => {
                    println!("no pending tasks");
                }
            }
        }

        Command::Done { id } => {
            store::mark_done(&tasks_path, &id)?;
            println!("done: {id}");
        }

        Command::Fail { id, reason } => {
            store::mark_failed(&tasks_path, &id, reason.as_deref())?;
            // mark_failed は defer_until を now + 172800 (2日後) に設定する。
            // 設定した defer_until を読み取って表示する。
            let tasks = store::load(&tasks_path)?;
            if let Some(task) = tasks.iter().find(|t| t.id == id) {
                if let Some(defer_until) = task.defer_until {
                    // defer_until を人が読める日時文字列に変換する
                    let secs = defer_until as u64;
                    let dt = format_unix_datetime(secs);
                    println!("failed: {id}");
                    println!("deferred until {dt} (2 日後に再実行されます)");
                } else {
                    println!("failed: {id}");
                }
            } else {
                println!("failed: {id}");
            }
        }

        Command::Edit {
            id,
            title,
            tags,
            notes,
            status,
        } => {
            let tags_opt = if tags.is_empty() { None } else { Some(tags) };
            store::edit(
                &tasks_path,
                &id,
                title.as_deref(),
                tags_opt,
                notes.as_deref(),
                status.as_deref(),
            )?;
            println!("updated: {id}");
        }

        Command::SessionStart => {
            run_hook(|| {
                let raw = read_stdin();
                // Don't silently coerce malformed stdin to an empty default —
                // that would run session-start against a blank input and could
                // mis-key the project. Surface it on stderr and skip (the hook
                // still exits 0, never breaking the turn).
                let Some(input) = HookInput::parse(&raw) else {
                    eprintln!("backlog session-start: malformed hook input on stdin; skipping");
                    return;
                };
                if let Some(ctx) = hooks::session_start::run(&input) {
                    println!("{}", json!({ "additionalContext": ctx }));
                }
            });
        }

        Command::Install { dry_run } => {
            install::install(dry_run)?;
        }

        Command::Uninstall { dry_run } => {
            install::uninstall(dry_run)?;
        }

        Command::Lock { action } => match action {
            LockAction::Acquire {
                session_id,
                project,
                force,
            } => {
                let pid = std::process::id();
                if force {
                    lock::acquire_forced(&session_id, pid, &project)?;
                    println!("lock acquired (forced)");
                } else {
                    lock::acquire(&session_id, pid, &project)?;
                    println!("lock acquired");
                }
            }
            LockAction::Release => {
                lock::release()?;
                println!("lock released");
            }
            LockAction::Status => {
                match lock::status() {
                    lock::LockStatus::None => println!("none"),
                    lock::LockStatus::Active(info) => {
                        println!("{}", serde_json::to_string_pretty(&info)?);
                    }
                    lock::LockStatus::Stale(info) => {
                        // Print the info with an extra stale field. `info`
                        // serializes to a JSON object, but stay fail-soft: if it
                        // ever isn't one, print it as-is rather than panicking
                        // (a `Stop`/CLI path must never abort on an unwrap).
                        let mut v = serde_json::to_value(&info)?;
                        if let Some(obj) = v.as_object_mut() {
                            obj.insert("stale".to_string(), serde_json::Value::Bool(true));
                        }
                        println!("{}", serde_json::to_string_pretty(&v)?);
                    }
                }
            }
        },
    }

    Ok(())
}

fn now_unix() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Unix タイムスタンプ (秒) を "YYYY-MM-DD HH:MM UTC" 形式の文字列に変換する。
/// 標準ライブラリのみで実装 (外部クレート不使用)。
fn format_unix_datetime(secs: u64) -> String {
    // グレゴリオ暦変換 (ユリウス通日ベース)
    let days = secs / 86400;
    let time_of_day = secs % 86400;
    let hh = time_of_day / 3600;
    let mm = (time_of_day % 3600) / 60;

    // 1970-01-01 からの日数を年月日に変換 (Fliegel-Van Flandern algorithm)
    let jd = days + 2440588; // Julian Day Number for 1970-01-01
    let l = jd + 68569;
    let n = 4 * l / 146097;
    let l = l - (146097 * n).div_ceil(4);
    let i = 4000 * (l + 1) / 1461001;
    let l = l - 1461 * i / 4 + 31;
    let j = 80 * l / 2447;
    let day = l - 2447 * j / 80;
    let l = j / 11;
    let month = j + 2 - 12 * l;
    let year = 100 * (n - 49) + i + l;

    format!(
        "{:04}-{:02}-{:02} {:02}:{:02} UTC",
        year, month, day, hh, mm
    )
}
