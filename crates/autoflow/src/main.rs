//! autoflow — session-end auto-flow gate for Claude Code.
//!
//! Stop hook state machine (per session):
//!   idle → [enough work?] → block: /record → record_requested
//!   record_requested | continuing → [condukt pending?]
//!     yes → block: /condukt (condukt tasks) → continuing
//!     no  → [backlog open?]
//!       yes → [compass charter fresh?]   (soft dep; absent ⇒ treated as fresh)
//!             fresh → block: /backlog <next item> → continuing
//!             stale → block: nudge /compass → done (stand down, don't drive)
//!       no  → done (allow)
//!   done → allow
//!
//!   condukt_prompts < 5  → block automatically
//!   condukt_prompts ≥ 5  → block: ask user each time
//!
//! Independently of the above (Tier 2 delegation-record advisory): on every
//! `record_requested`/`continuing` Stop, if this session's transcript shows
//! `/flow` drove a condukt run to completion without ever calling
//! `fugu-router record` for it, block once with a fail-soft nudge (deduped via
//! `SessionState::delegation_audit_warned`) — see `delegation_audit`.

mod backlog;
mod compass;
mod condukt;
mod config;
mod delegation_audit;
mod insights;
mod lock;
mod state;

use clap::{Parser, Subcommand};
use harness_core::hook::{read_stdin, run_hook, HookInput};
use serde_json::json;

use config::Config;
use state::Phase;

#[derive(Parser)]
#[command(
    name = "autoflow",
    version,
    about = "Session-end auto-flow gate for Claude Code."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Stop hook: run the record→condukt state machine.
    Stop,
    /// SessionStart hook: inject a /flow proposal when pending backlog items exist.
    /// Silent when queue is empty and charter is fresh — never breaks a turn.
    SessionStart,
    /// PreCompact hook: if the flow loop is running in THIS session (this session
    /// holds the backlog lock) and not opted out, drop a resume-flow marker so the
    /// next UserPromptSubmit re-injects a "/flow を再開" instruction. Never blocks
    /// compaction; silent when the gate isn't met.
    PreCompact,
    /// UserPromptSubmit hook: consume this session's resume-flow marker (if any)
    /// and inject the "/flow を再開" instruction exactly once after a /compact.
    /// Silent (no output) on every ordinary turn — zero noise.
    PromptSubmit,
}

fn main() {
    let cli = Cli::parse();
    match cli.command {
        Command::Stop => stop_command(),
        Command::SessionStart => session_start_command(),
        Command::PreCompact => pre_compact_command(),
        Command::PromptSubmit => prompt_submit_command(),
    }
}

/// Process-global guard for tests that mutate the `HOME` env var. Several test
/// modules (lock.rs, main.rs) read the backlog lock under `$HOME/.backlog`; cargo
/// runs a binary's tests concurrently, so they must serialize behind ONE mutex
/// (recovering from poison if a holder panics) to avoid a cross-test HOME race.
#[cfg(test)]
pub(crate) fn test_home_guard() -> std::sync::MutexGuard<'static, ()> {
    use std::sync::{Mutex, OnceLock};
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// The effective session id for a hook: the payload's `session_id`, else the
/// `CLAUDE_CODE_SESSION_ID` env var (mirrors `stop_command`). Empty means the
/// caller should stay silent (never key state on an unknown session).
fn resolve_session_id(input: &HookInput) -> String {
    if input.session_id.is_empty() {
        std::env::var("CLAUDE_CODE_SESSION_ID").unwrap_or_default()
    } else {
        input.session_id.clone()
    }
}

/// The Stop hook entry point. Reads stdin once and determines the panic-guard
/// mode flags up front, then runs the state-machine body under
/// `harness_core::gate::run::run_guarded`: a panic in `stop_run` (a crash
/// before any decision was emitted) now fails CLOSED (`decision:block`)
/// instead of the old behavior of silently exiting 0 with no decision
/// (indistinguishable from a clean, nothing-to-do stop).
fn stop_command() -> ! {
    let raw = read_stdin();
    let hook = HookInput::parse(&raw);
    let interactive = hook.is_none();
    let stop_hook_active = hook.as_ref().is_some_and(|h| h.stop_hook_active);
    harness_core::gate::run::run_guarded("autoflow", interactive, stop_hook_active, move || {
        stop_run(hook.unwrap_or_default())
    });
    std::process::exit(0);
}

fn stop_run(input: HookInput) {
    {
        let session_id = if input.session_id.is_empty() {
            std::env::var("CLAUDE_CODE_SESSION_ID").unwrap_or_default()
        } else {
            input.session_id.clone()
        };
        if session_id.is_empty() {
            return;
        }

        let cfg = Config::load();
        if !cfg.enabled || Config::disabled_env() {
            return;
        }

        let cwd = input.cwd_or_current();

        // Stand down while another live session holds the backlog lock for
        // THIS project: a /flow or /backlog driver is already running condukt
        // against this queue, and autoflow's auto-loop would double-drive it.
        // (The lock is per-project, so an unrelated project's driver must not
        // stand this session down.)
        if lock::backlog_driver_active(&cwd) {
            return;
        }
        let mut s = state::load(&cfg.state_dir, &session_id);

        match s.phase {
            Phase::Idle => {
                let metrics = insights::load_metrics(&session_id);
                if metrics.turns >= cfg.min_turns && metrics.tool_events >= cfg.min_tool_events {
                    s.phase = Phase::RecordRequested;
                    state::save(&cfg.state_dir, &session_id, &s);
                    block("/session-insights:record を実行してセッションを記録してください。");
                }
            }
            Phase::RecordRequested | Phase::Continuing => {
                // Tier 2 delegation-record advisory: independent of the
                // pending-emptiness branch below, fires at most once per
                // session (deduped via `delegation_audit_warned`).
                if !s.delegation_audit_warned
                    && delegation_audit::missing_delegation_record(&input.transcript_path, &cwd)
                {
                    s.delegation_audit_warned = true;
                    state::save(&cfg.state_dir, &session_id, &s);
                    block(
                        "/flow経由のcondukt実行が完了しましたが、fugu-router recordでのdelegation記録が見当たりません。\
`fugu-router record --class flow-delegation --delegation <fork|inline> ...`の呼び出しを確認してください。",
                    );
                    return;
                }

                let pending = condukt::find_pending(&cwd);
                if !pending.is_empty() {
                    s.condukt_prompts += 1;
                    s.phase = Phase::Continuing;
                    state::save(&cfg.state_dir, &session_id, &s);

                    // Mark tasks as running so interruptions can be detected.
                    let ids: Vec<&str> = pending.iter().map(|t| t.id.as_str()).collect();
                    condukt::mark_running(&cwd, &ids);

                    let list = pending
                        .iter()
                        .map(|t| format!("- {} ({})", t.id, t.status))
                        .collect::<Vec<_>>()
                        .join("\n");

                    if s.condukt_prompts <= 4 {
                        block(&format!(
                            "condukt に残課題が {} 件あります:\n{}\n\n/condukt で続きを処理してください。",
                            pending.len(),
                            list
                        ));
                    } else {
                        block(&format!(
                            "condukt に残課題が {} 件あります ({}回目):\n{}\n\n自動実行を停止しています。続けるかどうかユーザーに確認してください。",
                            pending.len(),
                            s.condukt_prompts,
                            list
                        ));
                    }
                } else {
                    // condukt 完了 → backlog を確認
                    let open = backlog::find_open(&cwd);
                    if open.is_empty() {
                        s.phase = Phase::Done;
                        state::save(&cfg.state_dir, &session_id, &s);
                    } else {
                        // About to auto-drive the backlog queue. Honor flow's
                        // invariant — never blind-drive a stale charter. Consult
                        // compass; if it reports the charter isn't sharp, nudge
                        // toward /compass and STAND DOWN instead of driving.
                        // compass is a soft dep: absent / unparseable => proceed
                        // as before (a repo that doesn't use compass is unaffected).
                        if let Some(v) = compass::charter_freshness(&cwd) {
                            if !v.fresh {
                                s.phase = Phase::Done;
                                state::save(&cfg.state_dir, &session_id, &s);
                                let why = v
                                    .reason
                                    .unwrap_or_else(|| "charter が鮮明ではありません".to_string());
                                block(&format!(
                                    "compass: {why}\n\n自動でバックログを流す前に /compass で再オリエンテーションしてください（鮮明化後に /flow か /backlog を再開）。"
                                ));
                                return;
                            }
                        }

                        s.backlog_prompts += 1;
                        // If we've hit the limit, give up — the skill or command likely failed.
                        if s.backlog_prompts > cfg.max_backlog_prompts {
                            s.phase = Phase::Done;
                            state::save(&cfg.state_dir, &session_id, &s);
                            return;
                        }
                        s.phase = Phase::Continuing;
                        state::save(&cfg.state_dir, &session_id, &s);

                        let next = &open[0];
                        let remaining = open.len();
                        let msg = format!(
                            "残課題バックログに {} 件の未完了課題があります。\n\n次の課題 [{}]: {}\n\n/backlog を実行してください。",
                            remaining, next.id, next.text
                        );
                        block(&msg);
                    }
                }
            }
            Phase::Done => {}
        }
    }
}

fn block(reason: &str) {
    println!("{}", json!({ "decision": "block", "reason": reason }));
}

fn session_start_command() -> ! {
    run_hook(|| {
        let raw = read_stdin();
        let input = HookInput::parse(&raw).unwrap_or_default();
        let cwd = input.cwd_or_current();

        // Check compass charter freshness before proposing backlog work.
        // Nudge toward /compass if the charter is stale — blind-driving a stale
        // charter is worse than staying silent.
        if let Some(v) = compass::charter_freshness(&cwd) {
            if !v.fresh {
                let why = v
                    .reason
                    .unwrap_or_else(|| "charter が鮮明ではありません".to_string());
                println!(
                    "{}",
                    json!({
                        "additionalContext": format!(
                            "compass: {why}\n/compass で再接地してから /flow を実行してください。"
                        )
                    })
                );
                return;
            }
        }

        // Check backlog for pending items and propose /flow when work exists.
        let open = backlog::find_open(&cwd);
        if !open.is_empty() {
            let n = open.len();
            let first = &open[0];
            println!(
                "{}",
                json!({
                    "additionalContext": format!(
                        "バックログに {} 件 (最優先: '{}')。/flow で開始しますか？",
                        n, first.text
                    )
                })
            );
        }
        // 0 pending + charter fresh → stay silent
    })
}

/// Instruction re-injected on the first prompt after a `/compact`, mirroring the
/// SessionStart `/flow` proposal wording. Kept as one const so the hook and its
/// test agree on the text.
const RESUME_FLOW_INJECT: &str = "直前に /compact したため flow ループを継続します: 中断した /flow の loop を次の一手から再開せよ（backlog lock は保持済み）。";

/// PreCompact core (testable): drop the resume-flow marker iff (a) the flow loop
/// is running in THIS session (this session holds the backlog lock) and (b) the
/// user hasn't opted out via `resume_flow_on_compact = false`. Any gate miss
/// writes nothing. Never panics, never blocks compaction.
fn pre_compact_run(session_id: &str, cwd: &std::path::Path, cfg: &Config) {
    if session_id.is_empty() {
        return; // unknown session → never key a marker
    }
    if !cfg.resume_flow_on_compact {
        return; // opted out
    }
    if !lock::this_session_holds_lock(session_id, cwd) {
        return; // flow loop is not driving THIS session → nothing to resume
    }
    state::write_resume_marker(&cfg.state_dir, session_id);
}

/// UserPromptSubmit core (testable): consume this session's resume-flow marker
/// and, if it existed, return the resume-/flow instruction to inject — exactly
/// once (the marker is deleted on consume). Returns None (stay silent) when there
/// is no marker or the session is unknown.
fn prompt_submit_run(session_id: &str, cfg: &Config) -> Option<String> {
    if session_id.is_empty() {
        return None;
    }
    if state::consume_resume_marker(&cfg.state_dir, session_id) {
        Some(RESUME_FLOW_INJECT.to_string())
    } else {
        None
    }
}

fn pre_compact_command() -> ! {
    run_hook(|| {
        let raw = read_stdin();
        let input = HookInput::parse(&raw).unwrap_or_default();
        let session_id = resolve_session_id(&input);
        let cwd = input.cwd_or_current();
        let cfg = Config::load();
        if !cfg.enabled || Config::disabled_env() {
            return;
        }
        pre_compact_run(&session_id, &cwd, &cfg);
    })
}

fn prompt_submit_command() -> ! {
    run_hook(|| {
        let raw = read_stdin();
        let input = HookInput::parse(&raw).unwrap_or_default();
        let session_id = resolve_session_id(&input);
        let cfg = Config::load();
        if !cfg.enabled || Config::disabled_env() {
            return;
        }
        // UserPromptSubmit injects whatever a hook writes to stdout on exit 0
        // (same channel as ctxrot's guard). Nothing is printed on ordinary turns.
        if let Some(msg) = prompt_submit_run(&session_id, &cfg) {
            println!("{msg}");
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::MutexGuard;

    // `this_session_holds_lock` now shells out to the real `backlog` binary
    // (per-project lock lookup) rather than reading a file directly, so the
    // "lock actually held by THIS session" case can't be exercised as a pure
    // unit test here without spawning both binaries — that case is covered
    // end-to-end in `tests/precompact_lock.rs` instead. Every test in this
    // module only reaches code paths that short-circuit *before* the lock
    // check (empty session id, opted-out config) or don't touch the lock at
    // all (prompt-submit consume), so none of them depend on `backlog` being
    // installed. They still mutate the process-global HOME, so they serialize
    // behind the crate-wide `test_home_guard` mutex to avoid a cross-test race.

    /// A temp HOME with `.backlog/` created, a `state/` dir for markers, and a
    /// `project/` dir to use as `cwd`. The TempDir self-cleans on drop;
    /// `_guard` releases the HOME mutex last.
    struct TmpEnv {
        _dir: tempfile::TempDir,
        project_dir: std::path::PathBuf,
        state_dir: std::path::PathBuf,
        _guard: MutexGuard<'static, ()>,
    }
    impl TmpEnv {
        fn new() -> Self {
            let guard = crate::test_home_guard();
            let dir = tempfile::tempdir().expect("tempdir");
            let home = dir.path().to_path_buf();
            std::fs::create_dir_all(home.join(".backlog")).unwrap();
            let project_dir = home.join("project");
            std::fs::create_dir_all(&project_dir).unwrap();
            let state_dir = home.join("state");
            std::env::set_var("HOME", &home);
            TmpEnv {
                _dir: dir,
                project_dir,
                state_dir,
                _guard: guard,
            }
        }
        fn cfg(&self, resume: bool) -> Config {
            Config {
                enabled: true,
                min_turns: 2,
                min_tool_events: 3,
                state_dir: self.state_dir.clone(),
                max_backlog_prompts: 2,
                resume_flow_on_compact: resume,
            }
        }
    }

    // No lock present (backlog likely absent in the test sandbox too) → no
    // marker. Covers the fail-soft path where `find_backlog_binary()` finds
    // nothing at all.
    #[test]
    fn precompact_writes_no_marker_without_a_held_lock() {
        let env = TmpEnv::new();
        let cfg = env.cfg(true);
        let sess = "sess-own";

        pre_compact_run(sess, &env.project_dir, &cfg);
        assert!(
            !state::resume_marker_path(&cfg.state_dir, sess).exists(),
            "no lock → no marker"
        );
    }

    // Opt-out: resume_flow_on_compact = false suppresses the marker before the
    // lock is ever consulted.
    #[test]
    fn precompact_respects_opt_out() {
        let env = TmpEnv::new();
        let cfg = env.cfg(false); // opted out
        let sess = "sess-optout";
        pre_compact_run(sess, &env.project_dir, &cfg);
        assert!(
            !state::resume_marker_path(&cfg.state_dir, sess).exists(),
            "opted out → no marker"
        );
    }

    // Consume exactly once: with a marker present, prompt_submit injects the
    // resume text and deletes the marker; a second call injects nothing.
    #[test]
    fn prompt_submit_consumes_marker_once() {
        let env = TmpEnv::new();
        let cfg = env.cfg(true);
        let sess = "sess-consume";
        state::write_resume_marker(&cfg.state_dir, sess);

        let first = prompt_submit_run(sess, &cfg).expect("marker present → injects");
        assert!(first.contains("/flow"), "injects a /flow resume: {first}");
        assert!(
            !state::resume_marker_path(&cfg.state_dir, sess).exists(),
            "marker consumed (deleted) on first inject"
        );
        assert!(
            prompt_submit_run(sess, &cfg).is_none(),
            "second call is silent (fires exactly once)"
        );
    }

    // Fail-soft: no marker / unknown session → both hooks stay silent and never
    // write anything (they must not panic; run_hook also catches panics).
    #[test]
    fn hooks_are_silent_without_marker_or_session() {
        let env = TmpEnv::new();
        let cfg = env.cfg(true);

        // No marker → no injection.
        assert!(prompt_submit_run("sess-none", &cfg).is_none());

        // Unknown (empty) session → both no-op.
        assert!(prompt_submit_run("", &cfg).is_none());
        pre_compact_run("", &env.project_dir, &cfg);
        assert!(
            !state::resume_marker_path(&cfg.state_dir, "").exists(),
            "empty session → no marker"
        );
    }
}
