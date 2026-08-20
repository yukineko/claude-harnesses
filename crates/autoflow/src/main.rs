//! autoflow — session-end auto-flow gate for Claude Code.
//!
//! Stop hook state machine (per session):
//!   idle → [enough work?] → block: /record → record_requested
//!   record_requested | continuing → [condukt pending?]
//!     yes → block: /condukt (condukt tasks) → continuing
//!     no  → done (allow)
//!   done → allow
//!
//! **The backlog queue is not part of this machine.** It was until 2026-08-20:
//! an empty condukt pending set fell through to `[backlog open?]`, which blocked
//! with "/backlog を実行してください" behind a compass-freshness gate, and a
//! SessionStart hook proposed "/flow で開始しますか？" for the same queue. Both
//! were RETIRED on the user's instruction — see the retirement notes at the
//! former call sites for what the change does and does not move. autoflow now
//! drives only a condukt run that is already in flight; deciding to work the
//! queue is the operator's call, made by invoking /flow or /backlog.
//!
//! Continuation is progress-based, not count-based: the condukt (pending) branch
//! calls `state::decide_progress`. While the set keeps shrinking, autoflow keeps
//! continuing (blocking) — there is no cumulative call-count ceiling. Only a
//! stalled-progress streak reaching `cfg.stuck_threshold` escalates, and even
//! then it stays VISIBLE (blocks with an escalation message, worded by autonomy)
//! rather than silently standing down. An empty pending set is one legitimate
//! stop (→ done/allow); the other is the user explicitly asking to stop THIS
//! Stop (see below) — every other path blocks.
//!
//! Independently of the above (Tier 2 delegation-record advisory): on every
//! `record_requested`/`continuing` Stop, if this session's transcript shows
//! `/flow` drove a condukt run to completion without ever calling
//! `fugu-router record` for it, block once with a fail-soft nudge (deduped via
//! `SessionState::delegation_audit_warned`) — see `delegation_audit`.
//!
//! Also independently: before any of the above runs, if the user's own most
//! recent transcript signal (a typed message, or their answer to an
//! `AskUserQuestion`) was an explicit stop instruction, this Stop is allowed
//! through untouched — no state transition, no block. This is a per-Stop
//! escape hatch, not a session-wide latch: it does not set `Phase::Done`, so
//! if the user resumes ordinary work later in the same session, autoflow's
//! normal nagging resumes exactly as before. See `stop_intent` for exactly
//! what counts as an explicit stop and why an absent/ambiguous signal never
//! triggers it.

mod backlog;
mod condukt;
mod config;
mod delegation_audit;
mod insights;
mod lock;
mod state;
mod stop_intent;

use clap::{Parser, Subcommand};
use harness_core::hook::{read_stdin, run_hook, HookInput};
use harness_core::verdict::{Determination, Verdict};
use serde_json::json;

use config::Config;
use state::{Phase, StopDecision};

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
    /// PreCompact hook: if the flow loop is running in THIS session (this session
    /// is a registered driver of the project) and not opted out, drop a
    /// resume-flow marker so the
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
        Command::PreCompact => pre_compact_command(),
        Command::PromptSubmit => prompt_submit_command(),
    }
}

/// Process-global guard for tests that mutate the `HOME` env var. Several test
/// modules (lock.rs, main.rs) read backlog's liveness state under `$HOME/.backlog`; cargo
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

        // Stand down while another live session is driving THIS project's
        // queue: a /flow or /backlog driver is already running condukt against
        // it, and autoflow's auto-loop would drive it a second time. Liveness
        // is per-project, so an unrelated project's driver must not stand this
        // session down; and a liveness answer we could not obtain counts as
        // "active" (see lock::backlog_driver_active).
        if lock::backlog_driver_active(&cwd) {
            return;
        }

        // Stand down for THIS Stop only when the user's own most recent
        // signal (a typed message, or their answer to an AskUserQuestion)
        // was an explicit stop instruction — see `stop_intent` for exactly
        // what counts as one and why an undetermined/absent signal never
        // triggers this. Deliberately NOT latched into `Phase::Done`: an
        // explicit stop acknowledges *this* Stop, not every future one in
        // the session, so if the user resumes ordinary work later, autoflow
        // resumes its normal nagging on backlog/condukt state exactly as
        // before.
        if stop_intent::user_requested_stop(&input.transcript_path) {
            return;
        }
        let mut s = state::load(&cfg.state_dir, &session_id);

        match s.phase {
            Phase::Idle => {
                let metrics = insights::load_metrics(&session_id);
                if metrics.turns >= cfg.min_turns && metrics.tool_events >= cfg.min_tool_events {
                    s.phase = Phase::RecordRequested;
                    state::save(&cfg.state_dir, &session_id, &s);
                    block(
                        &cwd,
                        &session_id,
                        "record-requested",
                        "/session-insights:record を実行してセッションを記録してください。",
                    );
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
                        &cwd,
                        &session_id,
                        "delegation-audit-missing",
                        "/flow経由のcondukt実行が完了しましたが、fugu-router recordでのdelegation記録が見当たりません。\
`fugu-router record --class flow-delegation --delegation <fork|inline> ...`の呼び出しを確認してください。",
                    );
                    return;
                }

                let pending = match condukt::find_pending(&cwd) {
                    Determination::Known(pending) => pending,
                    // We could not read the condukt run-state. Falling through
                    // to the backlog branch would let an unread run-state end
                    // in `Phase::Done` — the session would go permanently
                    // silent on the strength of an observation never made.
                    // Block instead, and leave the phase untouched so the next
                    // Stop re-observes rather than inheriting a verdict.
                    Determination::Undetermined(why) => {
                        block_undetermined(
                            &cwd,
                            &session_id,
                            "condukt-state-undetermined",
                            "condukt の run-state を読み取れませんでした",
                            why.as_str(),
                        );
                        return;
                    }
                };
                if !pending.is_empty() {
                    // Progress-based continuation: continue as long as the
                    // pending set is shrinking; escalate (visibly) only on a
                    // stalled-progress streak. No cumulative call-count ceiling.
                    let (decision, next_prev, next_streak) = state::decide_progress(
                        pending.len() as u32,
                        s.condukt_prev_pending,
                        s.condukt_no_progress_streak,
                        cfg.stuck_threshold,
                    );
                    s.condukt_prev_pending = next_prev;
                    s.condukt_no_progress_streak = next_streak;
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

                    match decision {
                        StopDecision::Continue => {
                            block(
                                &cwd,
                                &session_id,
                                "condukt-pending",
                                &format!(
                                    "condukt に残課題が {} 件あります:\n{}\n\n/condukt で続きを処理してください。",
                                    pending.len(),
                                    list
                                ),
                            );
                        }
                        StopDecision::EscalateStuck => {
                            // Stalled progress: never silent. Word the visible
                            // escalation by autonomy — autonomous keeps going
                            // (noting out-of-band handling), non-autonomous asks
                            // the user to confirm/redirect.
                            let reason = if is_autonomous() {
                                format!(
                                    "自律継続中: condukt の pending が {} 回連続で減っていません（進捗停滞を検知）。残課題 {} 件:\n{}\n\nout-of-band で対処しつつ継続します（/condukt を再実行）。",
                                    cfg.stuck_threshold,
                                    pending.len(),
                                    list
                                )
                            } else {
                                format!(
                                    "進捗が止まっています: condukt の pending が {} 回連続で減っていません。残課題 {} 件:\n{}\n\n継続するか方針を変えるかユーザーに確認してください。",
                                    cfg.stuck_threshold,
                                    pending.len(),
                                    list
                                )
                            };
                            block(&cwd, &session_id, "condukt-pending-stuck", &reason);
                        }
                        // pending is non-empty here, so decide_progress never
                        // returns DoneEmpty; handle defensively as Continue.
                        StopDecision::DoneEmpty => {
                            block(
                                &cwd,
                                &session_id,
                                "condukt-pending",
                                &format!(
                                    "condukt に残課題が {} 件あります:\n{}\n\n/condukt で続きを処理してください。",
                                    pending.len(),
                                    list
                                ),
                            );
                        }
                    }
                } else {
                    // The condukt run has no pending task. This is the one
                    // legitimate stop: allow it and latch.
                    //
                    // RETIRED 2026-08-20 (user instruction): what used to be
                    // here read the backlog queue and, if it was non-empty,
                    // BLOCKED the stop with "/backlog を実行してください" —
                    // every turn, until the queue emptied. A compass-freshness
                    // gate sat in front of it so a stale charter stood the
                    // driving down instead. Both are gone, along with the
                    // SessionStart "/flow で開始しますか？" proposal that said
                    // the same thing at the other end of the session.
                    //
                    // The latch's premise moved and is still an OBSERVATION,
                    // not an assumption: it was "condukt empty AND backlog
                    // empty", it is now "this condukt run has no pending
                    // task". The `Determination::Undetermined` arm on that
                    // observation blocks (above), so `Done` is never reached
                    // by failing to look. autoflow no longer reads the backlog
                    // queue at all on this path — the queue is the /flow and
                    // /backlog skills' business when the user invokes them.
                    s.phase = Phase::Done;
                    state::save(&cfg.state_dir, &session_id, &s);
                }
            }
            Phase::Done => {}
        }
    }
}

/// Whether the current run is autonomous, per `condukt state autonomy-check`.
/// Shells out (like `lock::backlog_driver_active`): exit 0 = autonomous; ANY
/// failure — non-zero exit, spawn error, missing binary — is treated as
/// non-autonomous (fail-safe: default to asking the user rather than assuming
/// autonomy). Consulted ONLY on the `EscalateStuck` path to word the visible
/// escalation message, so no extra subprocess is spawned on the common
/// progress (`Continue`) path.
fn is_autonomous() -> bool {
    std::process::Command::new("condukt")
        .args(["state", "autonomy-check"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|st| st.success())
        .unwrap_or(false)
}

fn block(cwd: &std::path::Path, session: &str, check_kind: &str, reason: &str) {
    emit_violation(cwd, session, check_kind);
    println!("{}", json!({ "decision": "block", "reason": reason }));
}

/// Block because autoflow could not make an observation it needs.
///
/// This is the crate's single "cannot determine" exit, routed through
/// `harness_core::verdict::Verdict`, whose `stop_decision()` maps `Undetermined`
/// onto the blocking channel exactly like a `Violation`. The alternative —
/// letting the Stop through — is not neutral: the surrounding state machine
/// reads "nothing to do" from exactly the same silence, so an unobserved queue
/// would end the session as if it had been observed empty.
///
/// The caller deliberately does NOT persist a phase before calling this: the
/// undetermined answer is about this tick only, and the next Stop must re-ask
/// rather than inherit a verdict. If the underlying cause is permanent, the
/// message names it (and `AUTOFLOW_DISABLE=1` remains the documented opt-out) —
/// a loud stop the operator can fix, rather than a silent one they cannot see.
fn block_undetermined(
    cwd: &std::path::Path,
    session: &str,
    check_kind: &str,
    what: &str,
    why: &str,
) {
    let verdict = Verdict::undetermined(format!(
        "{what}（判定不能）: {why}\n\n\
         これは「残作業が無い」という観測ではありません。原因を確認して解消してください\
         （恒久的に解消できない場合の緊急退避は AUTOFLOW_DISABLE=1）。"
    ));
    emit_violation(cwd, session, check_kind);
    match verdict.stop_decision() {
        Some(decision) => println!("{decision}"),
        // Unreachable: `Verdict::undetermined` is never `Clean`. Kept explicit
        // because falling silent here would reinstate the very fail-open this
        // function exists to close, so the impossible branch still blocks.
        None => println!(
            "{}",
            json!({
                "decision": "block",
                "reason": format!("{what}（判定不能）: {why}"),
            })
        ),
    }
}

/// Record a fleet-level violation for a blocking Stop, for cross-gate
/// correlated-error detection (`overwatch::violation`). Fail-soft: never
/// changes the gate's exit code/stdout, never panics if the overwatch store
/// is unwritable (mirrors donegate/reviewgate/tdd/budgetguard's
/// `emit_violation[s]`). `task_key` is set to the session id (see those
/// functions' doc comments for why: a Stop-hook gate has no separate "task"
/// concept below the session/turn it fires in).
fn emit_violation(cwd: &std::path::Path, session: &str, check_kind: &str) {
    let raw = overwatch::violation::RawViolation {
        check_kind: Some(check_kind),
        ..Default::default()
    };
    let event = overwatch::violation::build_event(
        overwatch::violation::ViolationSource::Autoflow,
        &raw,
        session.to_string(),
        session.to_string(),
        overwatch::store::now(),
        None,
    );
    if let Some(event) = event {
        let _ = overwatch::store::append_violation(cwd, &event);
    }
}

// RETIRED 2026-08-20 (user instruction): `session_start_command` used to be
// here, wired to the `SessionStart` hook. It read the backlog queue and, when
// items were pending, injected "バックログに {n} 件 (最優先: '{title}')。/flow で
// 開始しますか？"; when compass reported a stale charter it injected "/compass で
// 再接地してから /flow を実行してください。" instead. Both are gone with the
// hook, and so is the Stop hook's backlog arm that repeated the same request at
// the other end of the session.
//
// Nothing replaces it, deliberately. This was a PROPOSAL, not a gate: it had no
// verdict to preserve and nothing downstream read its silence as "the queue is
// empty" (the Stop hook re-observed on its own). So retiring it removes noise
// without moving any decision to the permissive side — which is why it can be
// deleted outright rather than degraded into a quieter nudge. The backlog crate
// still injects its own queue state at SessionStart; what is gone is the
// instruction to act on it.
//
// `autoflow prompt-submit` still re-injects a resume instruction after a
// `/compact`, but only for a flow loop ALREADY RUNNING in this session (gated on
// this session being a registered driver — see `pre_compact_run`). That is
// resuming work the user started, not proposing work they did not ask for, so it
// stays.

/// Instruction re-injected on the first prompt after a `/compact`, mirroring the
/// SessionStart `/flow` proposal wording. Kept as one const so the hook and its
/// test agree on the text.
const RESUME_FLOW_INJECT: &str = "直前に /compact したため flow ループを継続します: 中断した /flow の loop を次の一手から再開せよ（driver 登録は保持済み）。";

/// PreCompact core (testable): drop the resume-flow marker iff (a) the flow loop
/// is running in THIS session (this session is one of the project's registered
/// drivers, or holds the exclusive lock) and (b) the
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
                stuck_threshold: 3,
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

#[cfg(test)]
mod violation_emission_tests {
    use super::*;

    // Mutates the process-global HOME (to isolate `overwatch::store`'s
    // home-relative storage root), so it serializes behind the crate-wide
    // `test_home_guard` mutex like every other HOME-mutating test here.
    #[test]
    fn emit_violation_records_an_autoflow_event() {
        let _guard = crate::test_home_guard();
        let home = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", home.path());
        let root = tempfile::tempdir().unwrap();

        emit_violation(root.path(), "sess-1", "condukt-pending");
        let events = overwatch::store::scan_violations(root.path())
            .events_or_empty()
            .expect("read_violations");
        assert_eq!(events.len(), 1, "expected exactly one recorded violation");
        assert_eq!(
            events[0].source,
            overwatch::violation::ViolationSource::Autoflow
        );
        assert_eq!(events[0].signature, "autoflow:condukt-pending");

        std::env::remove_var("HOME");
    }

    #[test]
    fn emit_violation_with_blank_check_kind_records_nothing() {
        let _guard = crate::test_home_guard();
        let home = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", home.path());
        let root = tempfile::tempdir().unwrap();

        emit_violation(root.path(), "sess-1", "   ");
        let events = overwatch::store::scan_violations(root.path())
            .events_or_empty()
            .expect("read_violations");
        assert!(
            events.is_empty(),
            "a blank discriminator must not build a signature"
        );

        std::env::remove_var("HOME");
    }
}
