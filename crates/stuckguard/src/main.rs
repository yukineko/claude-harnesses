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
            // Build the message (which retrieves any PRIOR lesson) before
            // writing this escalation's own lesson, so a fresh escalation
            // never echoes back the lesson it is about to record itself.
            emitted = Some(message(t, escalated, count));
            if escalated {
                record_lesson(&session, t, count);
            }
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
        let mut out = format!(
            "{head}\n🛑 同じ試行を繰り返さないでください（通知 {count} 回目）。いったんツール呼び出しを止め、\
             (1) 何を試して何が失敗したか (2) 現在の仮説 (3) 判断に必要な情報 を簡潔に整理し、\
             **ユーザーに状況を報告して指示を仰いで**ください。"
        );
        if let Some(lesson_text) = retrieve_lesson(t) {
            out.push_str(&format!("\n📚 past lesson: {lesson_text}"));
        }
        out
    } else {
        format!(
            "{head}\n一歩引いて前提を疑い、別アプローチを検討してください。根本原因が不明なまま同じ操作を繰り返さないこと。\
             抜け出せない場合はユーザーに状況を共有して指示を仰いでください。"
        )
    }
}

/// On escalation, look up whether the cross-project lessons store
/// (`harness_core::lessons`) has a relevant past lesson for this stuck
/// pattern, and if so return its `lesson_text` to append to the escalation
/// message. Closes the write→retrieve loop: `record_lesson` writes on
/// escalation, this reads on the next one.
///
/// Fail-soft end to end: `lessons::load` never panics (missing/corrupt store
/// → empty Vec) and `lessons::search` never panics (empty store/query/no
/// overlap → empty Vec), so an empty store, no match, or an unreadable store
/// all fall through to `None` — the escalation message is then byte-identical
/// to the no-retrieval case.
fn retrieve_lesson(t: &Trip) -> Option<String> {
    let loaded = harness_core::lessons::load();
    let hits = harness_core::lessons::search(&t.detail, &loaded, 1);
    hits.into_iter().next().map(|m| m.lesson.lesson_text)
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

/// On escalation, distill the stuck pattern into a short, transferable
/// error-pattern lesson and append it to the cross-project lessons store
/// (`harness_core::lessons`) so `fugu-router lessons search` can surface it
/// on future tasks. Called ONLY when `escalated == true` — the non-escalating
/// nudge path never writes a lesson.
///
/// Fail-soft end to end: `lessons::append` is already void/fail-soft (a
/// corrupt/unwritable store silently drops the write), and every input used
/// to build the `Lesson` here is a plain, already-validated string/number —
/// nothing here can panic, so a stuck escalation can never break the hook.
fn record_lesson(session: &str, t: &Trip, count: u32) {
    use harness_core::lessons::{self, Kind as LessonKind, Lesson};
    use sha2::{Digest, Sha256};

    let kind_label = match t.kind {
        Kind::Repeat => "repeat",
        Kind::Oscillation => "oscillation",
    };

    let task_summary = format!("stuck pattern ({kind_label}): {}", t.detail);
    let lesson_text = match t.kind {
        Kind::Repeat if t.all_errored => format!(
            "When the same action ({}) fails and is repeated {} times in a row, \
             stop retrying it verbatim — the approach itself is wrong. Change strategy \
             (inspect the actual error, try a different tool/method, or ask the user) \
             instead of repeating the identical failing call.",
            t.detail, t.count
        ),
        Kind::Repeat => format!(
            "When the same action ({}) is repeated {} times without making progress, \
             stop and reassess: repeating an identical call rarely converges. Try a \
             different approach or escalate to the user with what was tried and why it \
             didn't work.",
            t.detail, t.count
        ),
        Kind::Oscillation => format!(
            "When edits to the same file ({}) keep undoing each other ({} reversals), \
             stop alternating between the two states — that's a sign of an unclear root \
             cause or a design conflict. Step back, diagnose why the fix keeps regressing, \
             or ask the user for direction instead of continuing to flip-flop.",
            t.detail, t.count
        ),
    };

    // Content-derived id (mirrors `fugu-router lessons add`): re-escalating on
    // the exact same pattern text is idempotent rather than growing the store
    // unboundedly across repeated sessions.
    let mut hasher = Sha256::new();
    hasher.update(b"error-pattern");
    hasher.update([0]);
    hasher.update(task_summary.as_bytes());
    hasher.update([0]);
    hasher.update(lesson_text.as_bytes());
    let id = format!("{:x}", hasher.finalize())[..16].to_string();

    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let lesson = Lesson {
        id,
        kind: LessonKind::ErrorPattern,
        task_summary,
        lesson_text,
        source_run: format!("stuckguard:{session}:{}", t.key),
        ts,
    };
    let _ = count; // reserved for future use (e.g. escalation tier in the text)
    lessons::append(&lesson);
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

    /// Guards tests that mutate the process-global `LESSONS_STORE_DIR` env var
    /// so they never race each other (this crate's tests run in parallel by
    /// default). Mirrors the precedent in `harness_core::store`'s tests.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn repeat_trip(detail: &str) -> Trip {
        Trip {
            key: format!("repeat:{detail}"),
            kind: Kind::Repeat,
            count: 3,
            all_errored: true,
            detail: detail.to_string(),
        }
    }

    /// Point `LESSONS_STORE_DIR` at a fresh isolated temp dir for the
    /// duration of the closure, restoring the previous value afterward.
    /// Never touches the real `~/.lessons` store.
    fn with_isolated_lessons_store<T>(f: impl FnOnce(&std::path::Path) -> T) -> T {
        const VAR: &str = "LESSONS_STORE_DIR";
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var(VAR).ok();

        let dir = std::env::temp_dir().join(format!(
            "stuckguard-lessons-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var(VAR, &dir);

        let result = f(&dir);

        match prev {
            Some(v) => std::env::set_var(VAR, v),
            None => std::env::remove_var(VAR),
        }
        let _ = std::fs::remove_dir_all(&dir);
        result
    }

    #[test]
    fn retrieve_lesson_appends_when_relevant_lesson_seeded() {
        with_isolated_lessons_store(|_dir| {
            use harness_core::lessons::{self, Kind as LessonKind, Lesson};

            let lesson = Lesson {
                id: "seed1".to_string(),
                kind: LessonKind::ErrorPattern,
                task_summary: "stuck pattern (repeat): cargo build retry loop".to_string(),
                lesson_text: "stop retrying cargo build verbatim, inspect the real error"
                    .to_string(),
                source_run: "stuckguard:sess:repeat:x".to_string(),
                ts: 1000,
            };
            lessons::append(&lesson);

            let trip = repeat_trip("cargo build retry loop");
            let msg = message(&trip, true, 2);
            assert!(
                msg.contains("stop retrying cargo build verbatim, inspect the real error"),
                "escalation message must contain the retrieved lesson_text: {msg}"
            );
            assert!(
                msg.contains("past lesson:"),
                "expected a 'past lesson:' line: {msg}"
            );
        });
    }

    #[test]
    fn retrieve_lesson_appends_nothing_when_store_empty_or_no_match() {
        with_isolated_lessons_store(|_dir| {
            let trip = repeat_trip("some totally unrelated pattern xyz");
            let msg_empty_store = message(&trip, true, 2);

            use harness_core::lessons::{self, Kind as LessonKind, Lesson};
            lessons::append(&Lesson {
                id: "unrelated".to_string(),
                kind: LessonKind::Convention,
                task_summary: "repo uses rustfmt".to_string(),
                lesson_text: "always run cargo fmt before commit".to_string(),
                source_run: "run-1".to_string(),
                ts: 1000,
            });
            let msg_no_match = message(&trip, true, 2);

            assert_eq!(
                msg_empty_store, msg_no_match,
                "no relevant lesson (empty store or no match) must not change the message"
            );
            assert!(
                !msg_empty_store.contains("past lesson:"),
                "message must be byte-identical to the no-retrieval case: {msg_empty_store}"
            );
        });
    }

    #[test]
    fn retrieve_lesson_unreadable_store_never_panics() {
        const VAR: &str = "LESSONS_STORE_DIR";
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var(VAR).ok();

        // Point the store dir at a path that can't be a directory (a file in
        // place of the expected dir), so any read attempt fails rather than
        // returning a normal empty/missing result.
        let bogus_parent = std::env::temp_dir().join(format!(
            "stuckguard-lessons-bogus-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&bogus_parent, b"not a directory").unwrap();
        // Use the file itself as the "store dir" — descending into it to read
        // lessons.jsonl must fail, not panic.
        std::env::set_var(VAR, &bogus_parent);

        let trip = repeat_trip("unreadable store pattern");
        let msg = message(&trip, true, 2);
        assert!(
            !msg.contains("past lesson:"),
            "unreadable store must degrade to no-retrieval, not panic: {msg}"
        );

        match prev {
            Some(v) => std::env::set_var(VAR, v),
            None => std::env::remove_var(VAR),
        }
        let _ = std::fs::remove_file(&bogus_parent);
    }

    #[test]
    fn non_escalated_message_never_retrieves_lesson() {
        with_isolated_lessons_store(|_dir| {
            use harness_core::lessons::{self, Kind as LessonKind, Lesson};

            let trip = repeat_trip("non escalated pattern");
            lessons::append(&Lesson {
                id: "seed2".to_string(),
                kind: LessonKind::ErrorPattern,
                task_summary: "stuck pattern (repeat): non escalated pattern".to_string(),
                lesson_text: "this must never show up on a non-escalated nudge".to_string(),
                source_run: "run-2".to_string(),
                ts: 1000,
            });

            let msg = message(&trip, false, 1);
            assert!(
                !msg.contains("past lesson:"),
                "non-escalation path must never attempt retrieval: {msg}"
            );
            assert!(
                !msg.contains("this must never show up"),
                "non-escalation path must never surface a lesson: {msg}"
            );
        });
    }
}
