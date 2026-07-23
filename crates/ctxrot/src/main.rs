//! ctxrot — a context-rot guard for Claude Code.
//!
//! One binary, one subcommand per hook. Hook subcommands read the event JSON
//! from stdin and emit the appropriate output. The cardinal rule: a hook must
//! NEVER break the user's turn — on any error we exit 0 and stay silent.

mod config;
mod eval;
mod glob;
mod hooks;
mod install;
mod loadset;
mod metrics;
mod usage;

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use config::Config;
use harness_core::hook::{read_stdin, run_hook, HookInput};
use harness_core::store::Store;

#[derive(Parser)]
#[command(
    name = "ctxrot",
    version,
    about = "Context-rot guard for Claude Code: detect, rescue, restore, distill."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// UserPromptSubmit hook: detect large refs + context-budget bands.
    Guard,
    /// PreCompact hook: rescue decisions/todos/files to a durable note.
    Rescue,
    /// SessionStart hook: inject a compact carryover from the latest note.
    Restore,
    /// PreToolUse hook: gate a Read of a pathologically large local file.
    Preguard,
    /// PostToolUse hook: warn on huge tool output.
    Toolguard,
    /// Stop hook: block the turn (ask Claude to run /compact) when context
    /// usage exceeds `auto_compact_at_percentage`. Exits 0 immediately when
    /// `stop_hook_active` is true (re-entry guard) or `auto_compact_enabled`
    /// is false (opt-in). Requires `auto_compact_enabled = true` in config.
    Stop,
    /// Merge ctxrot hooks into ~/.claude/settings.json.
    Install {
        #[arg(long)]
        dry_run: bool,
    },
    /// Remove ctxrot hooks from ~/.claude/settings.json.
    Uninstall {
        #[arg(long)]
        dry_run: bool,
    },
    /// Write a default ~/.ctxrot/config.toml and create store/state dirs.
    Init,
    /// Inspect the note store.
    Note {
        #[command(subcommand)]
        action: NoteAction,
    },
    /// Inspect the metrics log (per-session rollup of bands/notes/gates).
    Metrics {
        #[command(subcommand)]
        action: Option<MetricsAction>,
    },
    /// Offline recall eval: does re-anchor improve recall, at what token cost?
    Eval {
        #[command(subcommand)]
        action: EvalAction,
    },
    /// statusLine command: read the status JSON on stdin, print a one-line
    /// context-usage meter (`ctxrot 52% ▮▮▯▯▯ band1 ~104k/200k`).
    Statusline,
    /// Print the current session's context usage (for usage-aware /distill).
    /// Resolves the transcript from the session id unless --transcript is given.
    Usage {
        /// Transcript path to read (default: resolve from --session).
        #[arg(long)]
        transcript: Option<PathBuf>,
        /// Session id to resolve the transcript for (default: $CLAUDE_CODE_SESSION_ID).
        #[arg(long)]
        session: Option<String>,
    },
    /// Manage the per-project loadset: explicit control over what context to keep
    /// around (`pin`) and keep out (`drop`). Backs the `/ctx` skill.
    Ctx {
        #[command(subcommand)]
        action: CtxAction,
    },
    /// Internal: the DETACHED async-distill worker spawned by the PreCompact
    /// rescue when `distill_on_compact` is on. Runs `claude -p` on the
    /// pre-compaction transcript and writes a high-quality `distill-*` note. Not a
    /// hook — invoked by ctxrot itself; not for direct use.
    #[command(hide = true)]
    DistillBg {
        #[arg(long)]
        session: String,
        #[arg(long)]
        transcript: String,
        #[arg(long)]
        cwd: PathBuf,
    },
}

#[derive(Subcommand)]
enum CtxAction {
    /// Pin a path/label so `restore` re-surfaces it (as a pointer) each session.
    Pin {
        /// The path or label to pin.
        item: String,
        #[arg(long)]
        cwd: Option<PathBuf>,
    },
    /// Remove a path/label from the pinned set.
    Unpin {
        item: String,
        #[arg(long)]
        cwd: Option<PathBuf>,
    },
    /// Mark a path/label to keep OUT of context (advisory: honored on the next
    /// compaction / distill / fresh-session carryover — hooks can't evict live
    /// tokens, so a manual `/compact` realizes it immediately).
    Drop {
        item: String,
        #[arg(long)]
        cwd: Option<PathBuf>,
    },
    /// Remove a path/label from the dropped set.
    Undrop {
        item: String,
        #[arg(long)]
        cwd: Option<PathBuf>,
    },
    /// Show the project's loadset (pinned + dropped) and its file path.
    List {
        #[arg(long)]
        cwd: Option<PathBuf>,
    },
    /// Print the pinned items, one per line (machine-readable, for the skill).
    Pinned {
        #[arg(long)]
        cwd: Option<PathBuf>,
    },
    /// Print the dropped items, one per line (machine-readable, for the skill).
    Dropped {
        #[arg(long)]
        cwd: Option<PathBuf>,
    },
    /// Pin a specific note path so `restore` always uses it instead of
    /// auto-selecting the latest. Clear with `clear-note`.
    UseNote {
        /// Absolute path to the note file (from `ctxrot note list`).
        path: PathBuf,
        #[arg(long)]
        cwd: Option<PathBuf>,
    },
    /// Remove the pinned note, reverting `restore` to auto-selection.
    ClearNote {
        #[arg(long)]
        cwd: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum MetricsAction {
    /// Per-session rollup (default).
    Summary,
    /// Print the metrics log path.
    Path,
    /// A/B compare two session groups by id prefix (e.g. guard-on vs GUARD_DISABLE).
    /// Each prefix folds all matching sessions; prints both groups and Δ(A−B).
    Compare {
        /// Session-id prefix for group A (the standard protocol: guard ON).
        a: String,
        /// Session-id prefix for group B (the standard protocol: GUARD_DISABLE).
        b: String,
    },
    /// Print the peak context usage (% and max band) for a session id prefix.
    /// For /record to stamp "how close this session got" into the session note.
    Peak {
        /// Session-id prefix (pass $CLAUDE_CODE_SESSION_ID for the current one).
        session: String,
    },
}

#[derive(Subcommand)]
enum EvalAction {
    /// Generate recall cases + both prompt variants + a manifest into a dir.
    /// Feed each `*.on.txt`/`*.off.txt` to a model (see eval/run-recall.sh).
    Gen {
        /// Output directory for the cases + manifest.
        #[arg(long, default_value = "eval-cases")]
        out: PathBuf,
        /// Number of recall cases.
        #[arg(long, default_value_t = 9)]
        cases: usize,
        /// Chars of filler burying the planted decision (lost-in-the-middle).
        #[arg(long, default_value_t = 8000)]
        filler_chars: usize,
    },
    /// Score a results.jsonl (lines: {id, variant, answer}) against a manifest;
    /// prints accuracy per variant and the re-anchor added-token cost.
    Score {
        #[arg(long)]
        manifest: PathBuf,
        #[arg(long)]
        results: PathBuf,
    },
}

#[derive(Subcommand)]
enum NoteAction {
    /// List notes for a project (default: cwd), newest first.
    List {
        #[arg(long)]
        cwd: Option<PathBuf>,
    },
    /// Print the path of the latest note for a project.
    Latest {
        #[arg(long)]
        cwd: Option<PathBuf>,
    },
    /// Print (and create) the note directory for a project.
    Dir {
        #[arg(long)]
        cwd: Option<PathBuf>,
    },
    /// Write a note from stdin into the store; prints the path.
    Write {
        #[arg(long, default_value = "distill")]
        slug: String,
        #[arg(long)]
        cwd: Option<PathBuf>,
        /// Session id to tag the filename with, so the originating session can
        /// reach its own note amid parallel sessions (pass $CLAUDE_CODE_SESSION_ID).
        #[arg(long)]
        session: Option<String>,
        /// Enforce the distill contract: reject (exit 1, write nothing) unless the
        /// note carries the headings `restore` depends on (決定事項/Decisions and
        /// 残課題/Open todos). Use for /distill so carryover is never silently empty.
        #[arg(long)]
        require_sections: bool,
    },
    /// GC a project's note store: keep the newest N (config keep_notes_per_project)
    /// plus the newest keep_distill_min distill notes; delete older ones.
    Prune {
        #[arg(long)]
        cwd: Option<PathBuf>,
        /// Show what would be deleted without removing anything.
        #[arg(long)]
        dry_run: bool,
    },
}

/// Resolve a note-store listing for a CLI command, or refuse to print.
///
/// An unreadable store must not render as an empty one: `note list` would print
/// nothing (read as "this project has no notes"), and `note prune` would report
/// "0 notes removed, 0 kept" for a store it never saw. Neither has a useful
/// restricted output, so both say `unknown` on stderr and exit non-zero.
fn known_or_exit<T>(d: harness_core::verdict::Determination<T>) -> T {
    match d {
        harness_core::verdict::Determination::Known(v) => v,
        harness_core::verdict::Determination::Undetermined(why) => {
            eprintln!(
                "ctxrot: ノートストアを読めませんでした（「ノート無し」ではありません）: {why}"
            );
            std::process::exit(1);
        }
    }
}

fn main() {
    let cli = Cli::parse();
    match cli.command {
        Command::Guard => run_hook(|| {
            if Config::disabled() {
                return;
            }
            let raw = read_stdin();
            if let Some(input) = HookInput::parse(&raw) {
                let cfg = Config::load();
                if let Some(text) = hooks::guard::run(&input, &cfg) {
                    harness_core::inject_metrics::record(
                        "ctxrot",
                        &input.session_id,
                        &input.prompt,
                        text.chars().count(),
                    );
                    println!("{text}");
                }
            }
        }),
        Command::Rescue => run_hook(|| {
            if Config::disabled() {
                return;
            }
            let raw = read_stdin();
            if let Some(input) = HookInput::parse(&raw) {
                let cfg = Config::load();
                if let Some(path) = hooks::rescue::run(&input, &cfg) {
                    // PreCompact does not inject context; report to stderr only.
                    eprintln!("[ctxrot] rescue note saved: {}", path.display());
                }
                // Default-on (distill_on_compact): kick off a detached, high-quality
                // `claude -p` distill of the pre-compaction transcript. Fire-and-
                // forget — never blocks this 10s hook; the next guard re-injects it.
                hooks::distill::spawn_detached(&input, &cfg);
            }
        }),
        Command::Restore => run_hook(|| {
            if Config::disabled() {
                return;
            }
            let raw = read_stdin();
            if let Some(input) = HookInput::parse(&raw) {
                let cfg = Config::load();
                if let Some(text) = hooks::restore::run(&input, &cfg) {
                    // SessionStart: plain stdout is injected as additional context.
                    println!("{text}");
                }
            }
        }),
        Command::Preguard => run_hook(|| {
            if Config::disabled() {
                return;
            }
            let raw = read_stdin();
            if let Some(input) = HookInput::parse(&raw) {
                let cfg = Config::load();
                // `analyse` (not `run`) wraps the detector in a panic barrier so a
                // crash in the analysis maps to a deny, not a silent allow — see
                // hooks::preguard::analyse.
                if let Some(reason) = hooks::preguard::analyse(&input, &cfg) {
                    // PreToolUse: deny the call; the reason is the only steering
                    // channel (PreToolUse can't inject additionalContext).
                    let out = serde_json::json!({
                        "hookSpecificOutput": {
                            "hookEventName": "PreToolUse",
                            "permissionDecision": "deny",
                            "permissionDecisionReason": reason,
                        }
                    });
                    println!("{out}");
                }
            }
        }),
        Command::Toolguard => run_hook(|| {
            if Config::disabled() {
                return;
            }
            let raw = read_stdin();
            if let Some(input) = HookInput::parse(&raw) {
                let cfg = Config::load();
                // `analyse` (not `run`) wraps the detector in a panic barrier so a
                // crash in the analysis maps to a fail-closed placeholder/nudge,
                // not a silent pass-through of the un-analysed raw output — see
                // hooks::toolguard::analyse.
                let out_fields = hooks::toolguard::analyse(&input, &cfg);
                let mut hook_output = serde_json::Map::new();
                hook_output.insert(
                    "hookEventName".to_string(),
                    serde_json::Value::String("PostToolUse".to_string()),
                );
                if let Some(updated) = out_fields.updated {
                    hook_output.insert(
                        "updatedToolOutput".to_string(),
                        serde_json::Value::String(updated),
                    );
                }
                if let Some(nudge) = out_fields.nudge {
                    hook_output.insert(
                        "additionalContext".to_string(),
                        serde_json::Value::String(nudge),
                    );
                }
                // Only emit JSON when something beyond hookEventName was set.
                if hook_output.len() > 1 {
                    let out = serde_json::json!({ "hookSpecificOutput": hook_output });
                    println!("{out}");
                }
            }
        }),

        // ----- user-invoked (normal error reporting) -----
        Command::Stop => stop_command(),
        Command::Install { dry_run } => {
            if let Err(e) = install::install(dry_run) {
                eprintln!("install failed: {e}");
                std::process::exit(1);
            }
        }
        Command::Uninstall { dry_run } => {
            if let Err(e) = install::uninstall(dry_run) {
                eprintln!("uninstall failed: {e}");
                std::process::exit(1);
            }
        }
        Command::Init => {
            if let Err(e) = init() {
                eprintln!("init failed: {e}");
                std::process::exit(1);
            }
        }
        Command::Note { action } => {
            let cfg = Config::load();
            let store = Store::new(cfg.store_dir.clone());
            match action {
                NoteAction::List { cwd } => {
                    let cwd = cwd.unwrap_or_else(|| {
                        std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
                    });
                    for p in known_or_exit(store.list_notes(&cwd)) {
                        println!("{}", p.display());
                    }
                }
                NoteAction::Latest { cwd } => {
                    let cwd = cwd.unwrap_or_else(|| {
                        std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
                    });
                    match known_or_exit(store.latest_note(&cwd)) {
                        Some(p) => println!("{}", p.display()),
                        None => {
                            eprintln!("(no notes for this project)");
                            std::process::exit(1);
                        }
                    }
                }
                NoteAction::Dir { cwd } => {
                    let cwd = cwd.unwrap_or_else(|| {
                        std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
                    });
                    let dir = store.project_dir(&cwd);
                    if let Err(e) = std::fs::create_dir_all(&dir) {
                        eprintln!("could not create {}: {e}", dir.display());
                        std::process::exit(1);
                    }
                    println!("{}", dir.display());
                }
                NoteAction::Write {
                    slug,
                    cwd,
                    session,
                    require_sections,
                } => {
                    let cwd = cwd.unwrap_or_else(|| {
                        std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
                    });
                    let body = read_stdin();
                    if require_sections {
                        let missing = hooks::restore::missing_sections(&body);
                        if !missing.is_empty() {
                            eprintln!(
                                "distill contract violation — missing required section(s): {}",
                                missing.join(", ")
                            );
                            eprintln!(
                                "→ add the heading(s) (use \"_(なし / none)_\" if truly empty) and retry; nothing was written."
                            );
                            std::process::exit(1);
                        }
                        // Soft contract: the rest of the distill shape lifts carryover
                        // quality but isn't load-bearing for `restore`, so warn only.
                        let soft = hooks::restore::missing_recommended_sections(&body);
                        if !soft.is_empty() {
                            eprintln!(
                                "distill contract note — missing recommended section(s): {}",
                                soft.join(", ")
                            );
                            eprintln!(
                                "→ これらは restore には不要ですが carryover の質が上がります（任意・書き込みは継続）。"
                            );
                        }
                    }
                    let safe: String = slug
                        .chars()
                        .map(|c| {
                            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                                c
                            } else {
                                '-'
                            }
                        })
                        .collect();
                    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
                    // Tag with the session so restore can route this session back
                    // to its own note even with parallel sessions in one project.
                    let tag = harness_core::store::session_tag(session.as_deref().unwrap_or(""));
                    match store.write_note(&cwd, &format!("{safe}-{tag}-{stamp}"), &body) {
                        Ok(p) => println!("{}", p.display()),
                        Err(e) => {
                            eprintln!("write failed: {e}");
                            std::process::exit(1);
                        }
                    }
                }
                NoteAction::Prune { cwd, dry_run } => {
                    let cwd = cwd.unwrap_or_else(|| {
                        std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
                    });
                    let res = known_or_exit(store.prune(
                        &cwd,
                        cfg.keep_notes_per_project,
                        cfg.keep_distill_min,
                        dry_run,
                    ));
                    let verb = if dry_run { "would remove" } else { "removed" };
                    for p in &res.removed {
                        println!("{verb}: {}", p.display());
                    }
                    println!(
                        "{} note(s) {}, {} kept (limit {}, distill floor {})",
                        res.removed.len(),
                        if dry_run {
                            "would be removed"
                        } else {
                            "removed"
                        },
                        res.kept,
                        cfg.keep_notes_per_project,
                        cfg.keep_distill_min,
                    );
                }
            }
        }
        Command::Metrics { action } => {
            let cfg = Config::load();
            match action.unwrap_or(MetricsAction::Summary) {
                MetricsAction::Path => println!("{}", metrics::path(&cfg).display()),
                MetricsAction::Summary => {
                    let stats = metrics::summarize(&cfg);
                    if stats.is_empty() {
                        println!("(no metrics yet: {})", metrics::path(&cfg).display());
                    } else {
                        println!(
                            "{:<16} {:>7} {:>5} {:>4} {:>9} {:>9} {:>6} {:>4} {:>4}",
                            "session",
                            "prompts",
                            "cross",
                            "band",
                            "peak_tok",
                            "last_tok",
                            "rescue",
                            "gate",
                            "dump"
                        );
                        for s in &stats {
                            let sid: String = if s.session.chars().count() > 16 {
                                let t: String = s.session.chars().take(15).collect();
                                format!("{t}…")
                            } else {
                                s.session.clone()
                            };
                            println!(
                                "{:<16} {:>7} {:>5} {:>4} {:>9} {:>9} {:>6} {:>4} {:>4}",
                                sid,
                                s.prompts,
                                s.crossings,
                                s.max_band,
                                s.peak_tokens,
                                s.last_tokens,
                                s.rescues,
                                s.gates,
                                s.tooldumps
                            );
                        }
                    }
                }
                MetricsAction::Compare { a, b } => {
                    let stats = metrics::summarize(&cfg);
                    let ga = metrics::group_by_prefix(&stats, &a);
                    let gb = metrics::group_by_prefix(&stats, &b);
                    match (ga, gb) {
                        (None, _) => {
                            eprintln!("no session matches prefix '{a}' (group A)");
                            std::process::exit(1);
                        }
                        (_, None) => {
                            eprintln!("no session matches prefix '{b}' (group B)");
                            std::process::exit(1);
                        }
                        (Some((ga, na)), Some((gb, nb))) => {
                            let row = |label: &str, s: &metrics::SessionStat| {
                                println!(
                                    "{:<14} {:>7} {:>5} {:>4} {:>9} {:>6} {:>4} {:>4}",
                                    label,
                                    s.prompts,
                                    s.crossings,
                                    s.max_band,
                                    s.peak_tokens,
                                    s.rescues,
                                    s.gates,
                                    s.tooldumps
                                );
                            };
                            println!(
                                "{:<14} {:>7} {:>5} {:>4} {:>9} {:>6} {:>4} {:>4}",
                                "group",
                                "prompts",
                                "cross",
                                "band",
                                "peak_tok",
                                "rescue",
                                "gate",
                                "dump"
                            );
                            row(&format!("A:{a} ({na})"), &ga);
                            row(&format!("B:{b} ({nb})"), &gb);
                            // Occupancy shape (prompts spent per band): guard-ON
                            // should dwell less in the high bands than guard-OFF.
                            println!("dwell A:{a:>10}  {}", metrics::fmt_dwell(&ga.band_prompts));
                            println!("dwell B:{b:>10}  {}", metrics::fmt_dwell(&gb.band_prompts));
                            // ctxrot's own injection load (post-cap), the seed for
                            // a cross-harness injection budget (ADR 0001).
                            println!(
                                "inject  A:{a:>10}  {} chars   B:{b:>10}  {} chars",
                                ga.inject_chars, gb.inject_chars
                            );
                            // Δ(A−B): signed gaps on the figures the guard targets.
                            let d = |x: u64, y: u64| x as i64 - y as i64;
                            println!(
                                "{:<14} {:>7} {:>5} {:>4} {:>9} {:>6} {:>4} {:>4}",
                                "Δ A−B",
                                d(ga.prompts, gb.prompts),
                                d(ga.crossings, gb.crossings),
                                d(ga.max_band, gb.max_band),
                                d(ga.peak_tokens, gb.peak_tokens),
                                d(ga.rescues, gb.rescues),
                                d(ga.gates, gb.gates),
                                d(ga.tooldumps, gb.tooldumps),
                            );
                            println!(
                                "\n(標準プロトコルでは A=guard有効 / B=GUARD_DISABLE。\
                                 peak_tok と band の Δ が負ほどガードが context を抑えた証拠。)"
                            );
                        }
                    }
                }
                MetricsAction::Peak { session } => {
                    let stats = metrics::summarize(&cfg);
                    match metrics::group_by_prefix(&stats, &session) {
                        None => {
                            eprintln!("no session matches prefix '{session}'");
                            std::process::exit(1);
                        }
                        Some((g, n)) => {
                            let pct = usage::pct_from_tokens(&cfg, g.peak_tokens);
                            println!(
                                "peak ~{pct}% (band{}, ~{}/{} tokens, {n} session(s))",
                                g.max_band, g.peak_tokens, cfg.context_window
                            );
                        }
                    }
                }
            }
        }
        Command::Eval { action } => match action {
            EvalAction::Gen {
                out,
                cases,
                filler_chars,
            } => match eval::run_gen(&out, cases, filler_chars) {
                Ok(n) => {
                    println!(
                        "wrote {n} case(s) ×2 variants + manifest.json to {}",
                        out.display()
                    );
                    println!(
                        "next: feed each *.on.txt / *.off.txt to a model, then `ctxrot eval score`"
                    );
                    println!("(or run eval/run-recall.sh, which drives `claude -p` end-to-end)");
                }
                Err(e) => {
                    eprintln!("eval gen failed: {e}");
                    std::process::exit(1);
                }
            },
            EvalAction::Score { manifest, results } => {
                if let Err(e) = eval::run_score(&manifest, &results) {
                    eprintln!("eval score failed: {e}");
                    std::process::exit(1);
                }
            }
        },
        Command::Statusline => {
            // Never CRASH the status bar (always exit 0) — but never render a
            // blank/green line for an UNKNOWN state either. A cannot-determine
            // (a non-empty payload we can't parse, or a transcript we can't read)
            // is SURFACED as an explicit non-green "unknown" band, mirroring
            // `ctxrot usage`'s "不明" readout. Blank is reserved for "genuinely
            // nothing to report yet" (empty stdin / no session).
            let cfg = Config::load();
            let raw = read_stdin();
            match harness_core::hook::HookInput::parse(&raw) {
                Some(input) => {
                    if let Some(line) = statusline_from(&cfg, &input) {
                        println!("{line}");
                    }
                }
                None => {
                    // `parse` returns None for BOTH empty and unparseable stdin.
                    // Empty = nothing to render (blank is honest). Non-empty but
                    // unparseable = we were handed a payload and could not read it
                    // → cannot-determine → surface it, don't silently blank it.
                    if !raw.trim().is_empty() {
                        println!("{}", usage::unknown_line(&cfg));
                    }
                }
            }
        }
        Command::Usage {
            transcript,
            session,
        } => {
            let cfg = Config::load();
            let path = transcript
                .map(|p| p.to_string_lossy().into_owned())
                .or_else(|| {
                    let sid = session
                        .or_else(|| std::env::var("CLAUDE_CODE_SESSION_ID").ok())
                        .unwrap_or_default();
                    usage::find_transcript_for_session(&sid)
                        .map(|p| p.to_string_lossy().into_owned())
                });
            match path
                .as_deref()
                .and_then(harness_core::transcript::estimate_tokens)
            {
                Some((tokens, src)) => {
                    let pct = usage::pct_from_tokens(&cfg, tokens);
                    println!("{}", usage::line(&cfg, pct, Some(tokens), src == "bytes"));
                    println!("hint: {}", usage::hint(&cfg, pct));
                }
                None => {
                    println!("context使用量は不明（transcript 未解決）。focus 指定があれば distill を続行可。");
                }
            }
        }
        Command::Ctx { action } => {
            let cfg = Config::load();
            let resolve = |c: Option<PathBuf>| {
                c.unwrap_or_else(|| {
                    std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
                })
            };
            match action {
                CtxAction::Pin { item, cwd } => {
                    ctx_mutate(&cfg, resolve(cwd), "pinned", &item, |ls| ls.pin(&item))
                }
                CtxAction::Unpin { item, cwd } => {
                    ctx_mutate(&cfg, resolve(cwd), "unpinned", &item, |ls| ls.unpin(&item))
                }
                CtxAction::Drop { item, cwd } => {
                    ctx_mutate(&cfg, resolve(cwd), "dropped", &item, |ls| {
                        ls.drop_item(&item)
                    })
                }
                CtxAction::Undrop { item, cwd } => {
                    ctx_mutate(&cfg, resolve(cwd), "undropped", &item, |ls| {
                        ls.undrop(&item)
                    })
                }
                CtxAction::List { cwd } => {
                    let cwd = resolve(cwd);
                    let ls = loadset::LoadSet::load(&cfg.state_dir, &cwd);
                    println!(
                        "loadset: {}",
                        loadset::path_for(&cfg.state_dir, &cwd).display()
                    );
                    if ls.is_empty() {
                        println!("(空: このプロジェクトの pin / drop / use-note はまだありません)");
                        return;
                    }
                    if let Some(ref pref) = ls.preferred_note {
                        println!("preferred-note (restore が優先使用):");
                        println!("  * {pref}");
                    }
                    println!("pinned ({}):", ls.pinned.len());
                    for p in &ls.pinned {
                        println!("  + {p}");
                    }
                    println!("dropped ({}):", ls.dropped.len());
                    for d in &ls.dropped {
                        println!("  - {d}");
                    }
                    if !ls.dropped.is_empty() {
                        println!(
                            "\n（dropped はライブ context からは即時に消えません。\
                             `/compact` か新セッションで実効化されます）"
                        );
                    }
                }
                CtxAction::Pinned { cwd } => {
                    let ls = loadset::LoadSet::load(&cfg.state_dir, &resolve(cwd));
                    for p in &ls.pinned {
                        println!("{p}");
                    }
                }
                CtxAction::Dropped { cwd } => {
                    let ls = loadset::LoadSet::load(&cfg.state_dir, &resolve(cwd));
                    for d in &ls.dropped {
                        println!("{d}");
                    }
                }
                CtxAction::UseNote { path, cwd } => {
                    let path_str = path.to_string_lossy().into_owned();
                    ctx_mutate(&cfg, resolve(cwd), "preferred-note set", &path_str, |ls| {
                        ls.set_preferred_note(&path_str)
                    })
                }
                CtxAction::ClearNote { cwd } => {
                    let cwd = resolve(cwd);
                    let mut ls = loadset::LoadSet::load(&cfg.state_dir, &cwd);
                    if ls.clear_preferred_note() {
                        match ls.save(&cfg.state_dir, &cwd) {
                            Ok(_) => println!("preferred-note: cleared"),
                            Err(e) => {
                                eprintln!("loadset write failed: {e}");
                                std::process::exit(1);
                            }
                        }
                    } else {
                        println!("preferred-note: （変更なし: 設定されていません）");
                    }
                }
            }
        }
        Command::DistillBg {
            session,
            transcript,
            cwd,
        } => run_hook(move || {
            // Detached worker: respects the same global kill-switch and stays
            // silent on any failure (the rescue note is the safety net).
            if Config::disabled() {
                return;
            }
            let cfg = Config::load();
            hooks::distill::run_bg(&session, &transcript, &cwd, &cfg);
        }),
    }
}

/// The Stop hook entry point. Reads stdin once and determines the panic-guard
/// mode flags up front, then runs the actual logic under
/// `harness_core::gate::run::run_guarded`: a panic in `stop_run` (a crash
/// before any decision was emitted) now fails CLOSED (`decision:block`)
/// instead of the old behavior of silently exiting 0 with no decision
/// (indistinguishable from a clean stop).
fn stop_command() -> ! {
    let raw = read_stdin();
    let hook = HookInput::parse(&raw);
    let interactive = hook.is_none();
    let stop_hook_active = hook.as_ref().is_some_and(|h| h.stop_hook_active);
    harness_core::gate::run::run_guarded("ctxrot", interactive, stop_hook_active, move || {
        stop_run(hook)
    });
    std::process::exit(0);
}

fn stop_run(hook: Option<HookInput>) {
    let Some(input) = hook else {
        return;
    };
    let cfg = Config::load();
    if let Some((out, check_kind)) = hooks::stop::run(&input, &cfg) {
        println!("{out}");
        emit_violation(&input.cwd_or_current(), &input.session_id, check_kind);
    }
}

/// Record a fleet-level violation for a blocking Stop-hook nudge, for
/// cross-gate correlated-error detection (`overwatch::violation`). Fail-soft:
/// never changes the hook's exit code/stdout, never panics if the overwatch
/// store is unwritable (mirrors donegate's `emit_violations`,
/// crates/donegate/src/main.rs). `task_key` is set to the session id (see
/// that function's doc comment for why: a Stop-hook gate has no separate
/// "task" concept below the session/turn it fires in).
fn emit_violation(root: &std::path::Path, session: &str, check_kind: &str) {
    let raw = overwatch::violation::RawViolation {
        check_kind: Some(check_kind),
        ..Default::default()
    };
    let event = overwatch::violation::build_event(
        overwatch::violation::ViolationSource::Ctxrot,
        &raw,
        session.to_string(),
        session.to_string(),
        overwatch::store::now(),
        None,
    );
    if let Some(event) = event {
        let _ = overwatch::store::append_violation(root, &event);
    }
}

/// Load the project loadset, apply `f`, persist, and report. `f` returns whether
/// it changed anything (so we can say "（変更なし）" for a no-op). A write error
/// exits non-zero — these are user-invoked commands, not hooks.
fn ctx_mutate<F>(cfg: &Config, cwd: PathBuf, label: &str, item: &str, f: F)
where
    F: FnOnce(&mut loadset::LoadSet) -> bool,
{
    let mut ls = loadset::LoadSet::load(&cfg.state_dir, &cwd);
    let changed = f(&mut ls);
    match ls.save(&cfg.state_dir, &cwd) {
        Ok(_) => {
            if changed {
                println!("{label}: {item}");
            } else {
                println!("{label}: {item}（変更なし）");
            }
        }
        Err(e) => {
            eprintln!("loadset write failed: {e}");
            std::process::exit(1);
        }
    }
}

/// Build the status-bar line from a parsed hook payload. Prefers Claude's own
/// `context_window.used_percentage`; falls back to estimating from the transcript.
fn statusline_from(cfg: &Config, input: &harness_core::hook::HookInput) -> Option<String> {
    let pct = input
        .context_window
        .as_ref()
        .and_then(|c| c.used_percentage);
    let tokens = input.context_window.as_ref().and_then(|c| c.total_tokens());
    if let Some(p) = pct {
        // Claude's own reported % is authoritative — not an estimate.
        return Some(usage::line(cfg, p.round() as u64, tokens, false));
    }
    // No % from Claude. With no transcript path either, there is genuinely
    // nothing to report yet (no session) → a blank line is honest.
    if input.transcript_path.is_empty() {
        return None;
    }
    // We HAVE a transcript path but Claude gave no %. If we can estimate, render
    // it. If the estimate FAILS (the file exists in intent but can't be read /
    // stat'd), that is cannot-determine — surface an explicit non-green "unknown"
    // band, NOT a blank/green that reads as headroom (the mirror of the `usage`
    // subcommand's "不明" branch, and the fail-open this fix closes).
    match harness_core::transcript::estimate_tokens(&input.transcript_path) {
        // `src == "bytes"` is the rough size/4 proxy (no real usage block) — mark
        // it estimated so a low band isn't read as a confident reading.
        Some((t, src)) => Some(usage::line(
            cfg,
            usage::pct_from_tokens(cfg, t),
            Some(t),
            src == "bytes",
        )),
        None => Some(usage::unknown_line(cfg)),
    }
}

const SAMPLE_CONFIG: &str = r#"# ctxrot configuration
# store_dir can point at an Obsidian vault folder.
store_dir = "~/.ctxrot/store"
state_dir = "~/.ctxrot/state"

# Budget denominator for the % estimate. Set this to the EFFECTIVE CAP you want
# to stay under (the target), NOT your model's real context window. ctxrot is a
# "keep it under 200K" guard: leaving this at 200000 makes the 50/75/90% bands
# fire at ~100K/150K/180K. If you raise it to your real 1M window, the bands
# won't fire until ~950K and the whole point of the tool is lost.
context_window = 200000

# a local file at/above this many bytes counts as a "large reference"
large_file_bytes = 50000

# a tool output at/above this many bytes triggers the PostToolUse warning
huge_tool_output_bytes = 50000

# PreToolUse hard gate: an UNBOUNDED `Read` (no `limit`) of a local file at/above
# this many bytes is denied, steering the model to a sub-agent or a bounded slice.
# These are almost always logs/dumps/minified blobs. Set 0 to disable the gate.
gate_file_bytes = 1000000

# PreToolUse Bash gate (opt-in, default false). When true, deny Bash commands
# that are obviously unbounded dumps by their shape (`cat huge.log`, `journalctl`
# with no -n/--since, recursive `grep` with no -m, full `tail -n +1`, …) UNLESS a
# downstream bound (`| head`, `| wc`, `| sed -n`, `-m N`) caps the output. The
# heuristic is conservative; turn it on for Bash-heavy / sysadmin workloads.
gate_bash = false

# append one JSONL metrics line per hook event to <state_dir>/metrics.jsonl
# (budget trajectory, band crossings, note sizes, gate denies). Inspect with
# `ctxrot metrics`. Local only; set false (or env GUARD_METRICS=0) to disable.
metrics = true

# ascending fractions of the window that trigger escalating advice
bands = [0.50, 0.75, 0.90]

# Re-anchor (fights lost-in-the-middle): periodically re-surface THIS session's
# already-recorded Decisions/Open todos near the end of the window, where the
# model attends most. Conservative by design — only at/above reanchor_min_band,
# and at most once per reanchor_every_prompts qualifying prompts. Set
# reanchor_enabled=false to turn it off.
reanchor_enabled = true
reanchor_min_band = 2
reanchor_every_prompts = 8

# PDO session-anchor re-inject (§4.3): a SEPARATE track from the Decisions
# re-anchor above. When this session holds a live `overwatch` lease (a PDO unit —
# title + done_criteria), re-surface "what am I working on" at most once per this
# many qualifying prompts (band ≥ 1). Deliberately slower than reanchor_every_prompts
# (the anchor is one unchanging fact, not growing project knowledge). No live lease
# / overwatch not installed → silent (fail-soft, never breaks the turn).
anchor_reinject_every = 12

# Note-store GC (`ctxrot note prune`): keep at most keep_notes_per_project newest
# notes per project, but always protect the newest keep_distill_min distill notes
# (higher value than rescues) even if they fall outside that window.
keep_notes_per_project = 30
keep_distill_min = 10

# Coalescing: skip a *preemptive* (band-NN%) rescue write when this session
# already has a rescue note newer than this many seconds (PreCompact is never
# coalesced — it must always land before real loss). 0 disables coalescing.
rescue_coalesce_secs = 120

# Cap the guard's OWN per-turn injection (CJK-safe char count). At high band the
# large-ref + budget + anchor blocks can stack; left unbounded the guard becomes
# a rot source itself. Over the cap, blocks drop lowest-priority first
# (anchor → advice → safety). 0 disables the cap (inject every block in full).
guard_inject_max_chars = 1200

# --- load gate rules (rule-based allow/deny) ---------------------------------
# Glob patterns whose matching `Read` targets are ALWAYS denied, regardless of
# size — "never load these into main context". Wins over load_allow and the size
# gate. Patterns are path-aware: `*`/`?` stay within a segment, `**` crosses `/`,
# and a slash-less pattern (e.g. `*.log`) also matches the bare file name in any
# directory. Project-relative patterns (e.g. `secrets/**`) match absolute paths.
# env override: CTXROT_LOAD_DENY="**/*.log,secrets/**"
load_deny = []
# load_deny = ["**/*.log", "**/node_modules/**", "**/*.min.js", "secrets/**"]

# Glob patterns whose matching `Read` targets BYPASS the size gate — "explicitly
# trusted, load whole even if large". Applied only when load_deny didn't match.
# env override: CTXROT_LOAD_ALLOW="docs/**/*.md"
load_allow = []
# load_allow = ["docs/**/*.md"]

# Whether a load_deny match denies even when the Read carries an explicit `limit`
# (a bounded slice). true = a deny rule means "keep this out entirely", so even a
# slice is refused. false = let bounded slices of denied files through.
load_deny_even_with_limit = true

# --- auto-injection control (SessionStart carryover) -------------------------
# Master switch for the prior-session carryover injection (`restore`).
# env kill-switch: CTXROT_RESTORE_DISABLE=1
restore_enabled = true
# Which carryover sections to inject, and whether to surface pinned loadset items
# (from `/ctx pin`) as pointers at session start.
inject_decisions = true
inject_todos = true
inject_pinned = true

# --- async LLM distill on compaction -----------------------------------------
# On by default. Every /compact (manual or auto) — after the instant
# deterministic rescue note — spawns a DETACHED `claude -p` that distills the
# full pre-compaction transcript into a high-quality distill-* note. This never
# blocks compaction; the next prompt's guard re-injects the result so the
# post-compact context recovers (PreCompact/PostCompact can't inject). It spends
# one model call per compaction, on your session auth (subscription; no API key).
# Set false (or CTXROT_DISTILL_ON_COMPACT=0) to disable.
distill_on_compact = true
# Headless command for the distill (prompt on stdin, note markdown on stdout).
# A value with shell metachars runs via `sh -c`. env: CTXROT_DISTILL_CMD
distill_cmd = "claude -p"
# Wall-clock cap (seconds) for that background distill; on timeout the rescue
# note stands. The detached worker bears this wait, not the 10s hook.
# env: CTXROT_DISTILL_TIMEOUT_SECS
distill_timeout_secs = 180
# Proactively run the same background distill when usage first crosses into the
# top (danger) band — the ≈200k line — WITHOUT waiting for a /compact. Hooks
# can't trigger compaction, so this is how "auto-distill at 200k" works: heavy
# history is externalized to a distill-* note now and the next guard re-injects
# the summary (main trends toward 要約＋リンク). Real token release still needs
# /compact. Fires at most once per upward crossing. Spends one model call per
# crossing. Set false (or CTXROT_AUTO_DISTILL_ON_BAND=0) to disable.
auto_distill_on_band = true

# --- Stop-hook auto-compact nudge (feature ⑤) ---------------------------------
# When auto_compact_enabled = true, the `ctxrot stop` handler checks context
# usage on each Stop event. If usage exceeds auto_compact_at_percentage AND the
# stop is not itself a re-entry (stop_hook_active=false), it returns
# {"decision":"block", "reason":"..."} asking Claude to run /compact.
# The stop_hook_active guard prevents infinite loops: on the next Stop (after
# Claude responds) the hook sees stop_hook_active=true and exits 0 (allow).
#
# IMPORTANT: hooks cannot shell-out to /compact directly. This nudge causes
# Claude Code to continue the session so Claude itself can run /compact.
# Requires the `Stop` hook to be wired (ctxrot install, or manually in hooks.json).
#
# Default false (opt-in, so existing users are not surprised).
# env: CTXROT_AUTO_COMPACT=1
auto_compact_enabled = false
# Fraction of the context window (0.0–1.0) that triggers the nudge.
# Default 0.90 (90 %). env: CTXROT_AUTO_COMPACT_AT_PERCENTAGE
auto_compact_at_percentage = 0.90
"#;

fn init() -> anyhow::Result<()> {
    let cfg = Config::load();
    std::fs::create_dir_all(&cfg.store_dir)?;
    std::fs::create_dir_all(&cfg.state_dir)?;
    let path = Config::config_path();
    if path.exists() {
        println!("config already exists: {}", path.display());
    } else {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, SAMPLE_CONFIG)?;
        println!("wrote {}", path.display());
    }
    println!("store_dir: {}", cfg.store_dir.display());
    println!("state_dir: {}", cfg.state_dir.display());
    println!(
        "context_window: {} (= effective cap / target; NOT your model's real window — \
         keep it at the limit you want to stay under, e.g. 200000)",
        cfg.context_window
    );
    Ok(())
}

#[cfg(test)]
mod statusline_tests {
    use super::*;
    use harness_core::hook::{ContextWindow, HookInput};

    fn cfg() -> Config {
        Config::default()
    }

    /// When Claude supplies a %, render the concrete band (happy path).
    #[test]
    fn renders_band_from_claude_percentage() {
        std::env::set_var("NO_COLOR", "1");
        let input = HookInput {
            context_window: Some(ContextWindow {
                used_percentage: Some(52.0),
                ..Default::default()
            }),
            ..Default::default()
        };
        let line = statusline_from(&cfg(), &input).expect("a % must render a line");
        assert!(line.contains("52%"), "{line}");
        assert!(line.contains("band"), "{line}");
        assert!(!line.contains("unknown"), "{line}");
    }

    /// No % and no transcript path = genuinely no session yet → blank (None) is
    /// honest; this branch must NOT be forced to "unknown".
    #[test]
    fn no_signal_is_blank_not_unknown() {
        let input = HookInput {
            transcript_path: String::new(),
            context_window: None,
            ..Default::default()
        };
        assert!(
            statusline_from(&cfg(), &input).is_none(),
            "no session should stay blank, not fabricate a band"
        );
    }

    /// THE FIX: a transcript path is present but unreadable (here, a path that
    /// does not exist → `estimate_tokens` returns None). The OLD code did
    /// `estimate_tokens(..)?` → None → the caller printed NOTHING (a blank line
    /// that reads as healthy). It must now surface an explicit "unknown" band.
    /// Reverting the failure arm to `?`/None makes this assertion fail.
    #[test]
    fn unreadable_transcript_surfaces_unknown_not_blank() {
        std::env::set_var("NO_COLOR", "1");
        let input = HookInput {
            transcript_path: "/nonexistent/ctxrot-statusline-test/transcript.jsonl".to_string(),
            context_window: None,
            ..Default::default()
        };
        let line = statusline_from(&cfg(), &input)
            .expect("an unreadable transcript must fail closed to 'unknown', not blank (None)");
        assert!(line.contains("unknown"), "must be the unknown band: {line}");
        assert!(
            !line.contains("band0"),
            "must NOT read as a healthy low band: {line}"
        );
    }

    /// Byte-proxy fail-open (d85a173c): a transcript that EXISTS and is readable
    /// but has NO real `usage` block falls back to the `size/4` "bytes" proxy — a
    /// rough guess a corrupt/truncated transcript can make read low. The rendered
    /// band must carry the `est?` marker so a low reading is never taken as a
    /// confident "plenty of headroom". Reverting the caller to discard `_src`
    /// (drop the `est?` marker) makes this assertion fail.
    #[test]
    fn bytes_proxy_estimate_is_marked_not_confident() {
        use std::io::Write;
        std::env::set_var("NO_COLOR", "1");
        // A transcript with content but no `"usage"` object → estimate_tokens
        // returns ("bytes", size/4).
        let mut f = tempfile::NamedTempFile::new().unwrap();
        writeln!(
            f,
            "{{\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":\"hi\"}}}}"
        )
        .unwrap();
        let input = HookInput {
            transcript_path: f.path().to_string_lossy().into_owned(),
            context_window: None,
            ..Default::default()
        };
        let line = statusline_from(&cfg(), &input).expect("a readable transcript renders a line");
        assert!(
            line.contains("est?"),
            "a bytes-proxy estimate must be marked est?, not shown as a confident band: {line}"
        );
        assert!(
            !line.contains("unknown"),
            "a readable transcript is an estimate, not fully unknown: {line}"
        );
    }
}

#[cfg(test)]
mod violation_emission_tests {
    use super::emit_violation;
    use crate::config::HOME_ENV_LOCK;

    // Shares `config::HOME_ENV_LOCK` with `main.rs` (not a locally scoped
    // lock) — see that static's doc comment for why a per-module lock is not
    // enough to prevent cross-module races on this same env var.
    #[test]
    fn emit_violation_records_a_ctxrot_event() {
        let _guard = HOME_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev_home = std::env::var_os("HOME");
        let temp_home =
            std::env::temp_dir().join(format!("ctxrot-emit-violation-test-{}", std::process::id()));
        std::fs::create_dir_all(&temp_home).expect("create temp HOME");
        std::env::set_var("HOME", &temp_home);

        let root = temp_home.join("project");
        std::fs::create_dir_all(&root).expect("create project root");
        emit_violation(&root, "session-x", "budget-threshold-crossed");

        let events = overwatch::store::read_violations(&root).expect("read_violations");

        match prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        let _ = std::fs::remove_dir_all(&temp_home);

        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].source,
            overwatch::violation::ViolationSource::Ctxrot
        );
        assert_eq!(events[0].signature, "ctxrot:budget-threshold-crossed");
    }

    #[test]
    fn emit_violation_with_empty_check_kind_records_nothing() {
        let _guard = HOME_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev_home = std::env::var_os("HOME");
        let temp_home = std::env::temp_dir().join(format!(
            "ctxrot-emit-violation-empty-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&temp_home).expect("create temp HOME");
        std::env::set_var("HOME", &temp_home);

        let root = temp_home.join("project");
        std::fs::create_dir_all(&root).expect("create project root");
        emit_violation(&root, "session-x", "");

        let events = overwatch::store::read_violations(&root).expect("read_violations");

        match prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        let _ = std::fs::remove_dir_all(&temp_home);

        assert!(events.is_empty());
    }
}
