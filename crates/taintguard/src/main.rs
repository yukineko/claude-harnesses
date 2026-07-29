// テスト内の unwrap/expect/panic は意図的な assert であって fail-open ではないので許可する。
// production 側は workspace の [workspace.lints.clippy] で deny のまま。
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
//! taintguard — a Claude Code hook trio implementing a provenance-scoped
//! least-privilege gate.
//!
//! Contract (shared by every plugin in this repo): a hook must NEVER break the
//! user's turn. All three subcommands read a hook payload from stdin and
//! always exit 0 (`harness_core::hook::run_hook`).
//!
//! * `mark`  (PostToolUse, matcher `WebFetch|WebSearch|Read`) — after a tool
//!   call that may have introduced untrusted-provenance content into context,
//!   record this session as tainted.
//! * `gate`  (PreToolUse, matcher `Bash|Write|Edit|MultiEdit|NotebookEdit`) —
//!   before a write-class tool runs, if this session is tainted, emit the
//!   blastguard-style `ask` (interactive) / `deny` (headless) decision instead
//!   of staying silent (an allow).
//! * `clear` (Stop) — a clean turn ends: reset the taint marker so the next
//!   turn starts trusted again.
//!
//! Both `mark` and `gate` run their real logic behind a `catch_unwind` panic
//! barrier that resolves a panic to the FAIL-CLOSED outcome (a forced taint
//! mark; a forced ask/deny) rather than letting it unwind into `run_hook`'s
//! outer catch, which would silently exit 0 with no mark/no decision — i.e. an
//! allow. This mirrors `blastguard::main::analyse` / `ctxrot`'s
//! `preguard`/`toolguard` `analyse` barriers.

use clap::{Parser, Subcommand};

use harness_core::hook::{read_stdin, run_hook, HookInput};
use taintguard::{classify, hookio, interactive, observe, state};

#[derive(Parser)]
#[command(
    name = "taintguard",
    version,
    about = "Provenance-scoped least-privilege gate for Claude Code."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// PostToolUse hook (matcher `WebFetch|WebSearch|Read`): record taint.
    Mark,
    /// PreToolUse hook (matcher `Bash|Write|Edit|MultiEdit|NotebookEdit`):
    /// ask/deny when this session is tainted.
    Gate,
    /// Stop hook: clear the taint marker after a clean turn.
    Clear,
}

fn main() {
    let cli = Cli::parse();
    match cli.command {
        Command::Mark => run_hook(|| {
            let raw = read_stdin();
            if let Some(input) = HookInput::parse(&raw) {
                analyse_mark(&input);
            }
        }),
        Command::Gate => run_hook(|| {
            let raw = read_stdin();
            if let Some(input) = HookInput::parse(&raw) {
                emit_gate(&input, analyse_gate(&input));
            }
        }),
        Command::Clear => run_hook(|| {
            let raw = read_stdin();
            if let Some(input) = HookInput::parse(&raw) {
                let cwd = input.cwd_or_current();
                if let Err(reason) = state::clear(&cwd, &input.session_id) {
                    eprintln!(
                        "[taintguard] clear failed (staying tainted, the safe side): {reason}"
                    );
                }
            }
        }),
    }
}

// ---------------------------------------------------------------------------
// mark
// ---------------------------------------------------------------------------

/// Core `mark` decision, pure given `input` (all I/O is inside `state::mark`).
fn decide_mark(input: &HookInput) -> Result<(), String> {
    let cwd = input.cwd_or_current();
    let session = input.session_id.as_str();
    match input.tool_name.as_str() {
        "WebFetch" | "WebSearch" => state::mark(&cwd, session, "web"),
        "Read" => match input.target() {
            Some(target) => match classify::classify(&cwd, &target) {
                classify::Trust::Trusted => Ok(()),
                classify::Trust::Untrusted | classify::Trust::Indeterminate => {
                    state::mark(&cwd, session, "external-read")
                }
            },
            // A Read with no extractable file_path is indeterminate — fail
            // closed the same as an indeterminate path, not a silent no-op.
            None => state::mark(&cwd, session, "external-read"),
        },
        _ => Ok(()),
    }
}

/// Run [`decide_mark`] behind a panic barrier: a panic anywhere in the
/// analysis (classification, serialization) must not fall through to `main`'s
/// outer `run_hook` backstop, which would silently exit 0 with NO mark
/// written — the exact fail-open this barrier exists to prevent (mirrors
/// `blastguard::main::analyse` / `ctxrot::hooks::toolguard::analyse`). On a
/// caught panic we force a mark with source `"internal-error"` so the `gate`
/// subcommand treats the rest of this turn as tainted rather than clean.
fn analyse_mark(input: &HookInput) {
    let cwd = input.cwd_or_current();
    let session = input.session_id.clone();
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| decide_mark(input))) {
        Ok(Ok(())) => {}
        Ok(Err(reason)) => {
            eprintln!("[taintguard] mark failed: {reason}");
        }
        Err(_) => {
            eprintln!(
                "[taintguard] internal error while analysing a tool call; failing closed (marking tainted)"
            );
            if let Err(reason) = state::mark(&cwd, &session, "internal-error") {
                eprintln!("[taintguard] fail-closed mark also failed: {reason}");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// gate
// ---------------------------------------------------------------------------

/// Build the `ask`/`deny` decision line for `reason`, hardening to `deny`
/// exactly like `blastguard::model::Decision::hardened` when no human is
/// affirmatively detected (`interactive::ask_available`).
fn build_decision(reason: &str) -> String {
    if interactive::ask_available() {
        hookio::ask_json(reason)
    } else {
        hookio::deny_json(reason)
    }
}

fn format_sources(sources: &[String]) -> String {
    if sources.is_empty() {
        "an unspecified source".to_string()
    } else {
        sources.join(", ")
    }
}

/// What `gate` decided to DO — as distinct from what the check FOUND
/// (`state::Check`).
///
/// Three actions, and the middle two are **not** interchangeable with the
/// first. `Silent` is reachable only from `Check::Clean`; a `Tainted` or
/// `Undetermined` check always produces either `Enforce` or `Observe`, never
/// `Silent`. That is what keeps observe-only from being a fail-open: the
/// suppression is carried in its own variant, along with the finding it
/// suppressed, so nothing downstream can read it as "nothing was found".
#[derive(Debug, Clone, PartialEq, Eq)]
enum GateAction {
    /// The check came back `Clean`. Print nothing at all — byte-identical to
    /// this crate's behaviour before observe-only existed.
    Silent,
    /// A finding, and this process is in the enforce posture: print the
    /// `ask`/`deny` PreToolUse decision.
    Enforce(String),
    /// A finding, and this process is in the observe-only posture: print an
    /// `additionalContext` warning carrying **no** `permissionDecision`, and
    /// append `record` to the ledger so the suppressed enforcement is counted.
    Observe {
        context: String,
        record: observe::Record,
    },
}

/// Core `gate` decision, pure given `input`, the on-disk taint marker, and the
/// process posture. All I/O is inside `state::check` (the ledger append happens
/// later, in [`emit_gate`], so this stays a decision function).
fn decide_gate(input: &HookInput) -> GateAction {
    decide_gate_with(input, observe::posture())
}

/// [`decide_gate`] with the posture injected, so tests can drive both postures
/// without mutating the process-global environment.
fn decide_gate_with(input: &HookInput, posture: observe::Posture) -> GateAction {
    let cwd = input.cwd_or_current();
    let tool = input.tool_name.as_str();
    match state::check(&cwd, &input.session_id) {
        // A clean check is the ONLY route to silence, in either posture.
        state::Check::Clean => GateAction::Silent,
        state::Check::Tainted(sources) => {
            let desc = format_sources(&sources);
            match posture {
                observe::Posture::Enforce => GateAction::Enforce(build_decision(&format!(
                    "[taintguard] this turn consumed untrusted-provenance content ({desc}); \
                     write-class tools are downgraded until this turn ends cleanly \
                     (a clean Stop restores normal access)."
                ))),
                observe::Posture::ObserveOnly => GateAction::Observe {
                    context: observe::warning(&desc, tool),
                    record: observe::Record::now(tool, &sources, "tainted", &input.session_id),
                },
            }
        }
        state::Check::Undetermined(why) => match posture {
            observe::Posture::Enforce => GateAction::Enforce(build_decision(&format!(
                "[taintguard] could not verify this session's taint state ({why}); \
                 failing closed (treating this turn as tainted)."
            ))),
            observe::Posture::ObserveOnly => GateAction::Observe {
                context: observe::warning_undetermined(&why, tool),
                // No sources: an unreadable marker cannot name what tainted it.
                // Recorded as `undetermined` rather than folded into `tainted`
                // so a store-health problem stays visible in the tally.
                record: observe::Record::now(tool, &[], "undetermined", &input.session_id),
            },
        },
    }
}

/// Run [`decide_gate`] behind a panic barrier: a panic in the taint check must
/// not fall through to a silent allow. A caught panic resolves to the same
/// fail-closed ask/deny as `Check::Undetermined`.
///
/// The panic arm **enforces even in observe-only mode**, deliberately. A panic
/// means the analysis did not complete, so this process cannot claim to know
/// either the taint state or — since the posture read is itself inside the
/// barrier — the posture it was supposed to honour. Observe-only is an
/// affordance for measuring a *working* gate, not a licence to swallow an
/// internal error; cannot-determine resolves to the restricted side regardless
/// of posture (CLAUDE.md §3). A panic is therefore the one case where
/// observe-only does not suppress, and it is loud in stderr besides.
fn analyse_gate(input: &HookInput) -> GateAction {
    analyse_gate_barrier(input, decide_gate)
}

/// The panic barrier itself, with the decision function injected.
///
/// Parameterising it (rather than inlining `decide_gate` and having tests
/// re-implement the barrier in a look-alike helper) means the panic-barrier
/// tests exercise **this** code — the code that actually runs in production —
/// instead of a copy that could drift away from it. The previous shape had a
/// duplicated `analyse_gate_with` in the test module, so a regression in the
/// real barrier would not have failed any test.
fn analyse_gate_barrier<F>(input: &HookInput, f: F) -> GateAction
where
    F: FnOnce(&HookInput) -> GateAction,
{
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(input))) {
        Ok(out) => out,
        Err(_) => GateAction::Enforce(build_decision(
            "[taintguard] internal error while checking taint state; failing closed (ask/deny). \
             (Observe-only, if set, is NOT honoured on this path: a panic means the taint state \
             could not be determined, which always resolves to the restricted side.)",
        )),
    }
}

/// Emit `action`: print the hook line (if any) and, for a suppressed
/// enforcement, append the ledger record.
///
/// A ledger append failure is logged to stderr and does not change the emitted
/// decision — the warning has already been printed, so the event is never
/// silently invisible even when the durable record is lost. It also cannot
/// become a permission fail-open: the ledger is never read to decide whether to
/// enforce (see `observe::append`'s docs).
fn emit_gate(input: &HookInput, action: GateAction) {
    match action {
        GateAction::Silent => {}
        GateAction::Enforce(line) => println!("{line}"),
        GateAction::Observe { context, record } => {
            println!("{}", hookio::context_json(&context));
            let cwd = input.cwd_or_current();
            if let Err(reason) = observe::append(&cwd, &record) {
                eprintln!(
                    "[taintguard] observe-only: suppressed an enforcement but could NOT record it \
                     ({reason}); the measurement under-counts by at least one event"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn hook_input(
        tool: &str,
        tool_input: serde_json::Value,
        cwd: &std::path::Path,
        session: &str,
    ) -> HookInput {
        HookInput {
            tool_name: tool.to_string(),
            tool_input: Some(tool_input),
            cwd: cwd.to_string_lossy().into_owned(),
            session_id: session.to_string(),
            ..Default::default()
        }
    }

    /// `TAINTGUARD_STATE_DIR` is a process-global env var; tests that set it
    /// must not run concurrently with each other (`cargo test` parallelizes
    /// by default within one binary). Every test below holds this for its
    /// whole body via the returned guard.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn temp_env(
        name: &str,
    ) -> (
        std::sync::MutexGuard<'static, ()>,
        tempfile::TempDir,
        std::path::PathBuf,
    ) {
        let guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::Builder::new()
            .prefix(&format!("taintguard-main-{name}-"))
            .tempdir()
            .expect("tempdir");
        std::env::set_var("TAINTGUARD_STATE_DIR", dir.path());
        let cwd = dir.path().join("project");
        std::fs::create_dir_all(&cwd).unwrap();
        (guard, dir, cwd)
    }

    /// Enforce posture, passed explicitly everywhere below. The pre-existing
    /// tests used to call `decide_gate`, which reads the real process
    /// environment — so once `TAINTGUARD_OBSERVE_ONLY=1` exists, an operator
    /// with that var exported would have flipped these assertions. Injecting the
    /// posture keeps them deterministic.
    const ENFORCE: observe::Posture = observe::Posture::Enforce;

    /// Assert `action` is an enforced `ask` or `deny`.
    fn assert_enforced(action: &GateAction) {
        let line = match action {
            GateAction::Enforce(line) => line,
            other => panic!("expected an enforced decision, got {other:?}"),
        };
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(v["hookSpecificOutput"]["hookEventName"], "PreToolUse");
        assert!(
            v["hookSpecificOutput"]["permissionDecision"] == "ask"
                || v["hookSpecificOutput"]["permissionDecision"] == "deny",
            "expected ask/deny, got {v}"
        );
    }

    #[test]
    fn webfetch_marks_and_gate_then_asks_or_denies() {
        let (_guard, _dir, cwd) = temp_env("webfetch");
        let session = "s-webfetch";
        let mark_input = hook_input(
            "WebFetch",
            json!({"url": "https://example.com"}),
            &cwd,
            session,
        );
        decide_mark(&mark_input).unwrap();

        let gate_input = hook_input("Bash", json!({"command": "echo hi"}), &cwd, session);
        assert_enforced(&decide_gate_with(&gate_input, ENFORCE));
    }

    #[test]
    fn in_repo_read_does_not_mark() {
        let (_guard, _dir, cwd) = temp_env("inrepo-read");
        let session = "s-inrepo";
        let f = cwd.join("src.rs");
        std::fs::write(&f, "fn main() {}").unwrap();
        let mark_input = hook_input(
            "Read",
            json!({"file_path": f.to_string_lossy()}),
            &cwd,
            session,
        );
        decide_mark(&mark_input).unwrap();

        let gate_input = hook_input("Write", json!({"file_path": "out.rs"}), &cwd, session);
        assert_eq!(
            decide_gate_with(&gate_input, ENFORCE),
            GateAction::Silent,
            "an in-repo Read must not taint the session"
        );
    }

    #[test]
    fn external_read_marks_and_gate_blocks() {
        let (_guard, _dir, cwd) = temp_env("external-read");
        let session = "s-external";
        let outside = tempfile::Builder::new()
            .prefix("taintguard-external-")
            .tempdir()
            .unwrap();
        let secret = outside.path().join("secret.txt");
        std::fs::write(&secret, "s").unwrap();
        let mark_input = hook_input(
            "Read",
            json!({"file_path": secret.to_string_lossy()}),
            &cwd,
            session,
        );
        decide_mark(&mark_input).unwrap();

        let gate_input = hook_input("Edit", json!({"file_path": "src.rs"}), &cwd, session);
        assert_enforced(&decide_gate_with(&gate_input, ENFORCE));
    }

    #[test]
    fn clean_session_gate_allows_silently() {
        let (_guard, _dir, cwd) = temp_env("clean-gate");
        let gate_input = hook_input("Bash", json!({"command": "cargo test"}), &cwd, "s-clean");
        assert_eq!(decide_gate_with(&gate_input, ENFORCE), GateAction::Silent);
    }

    #[test]
    fn clear_restores_allow_after_mark() {
        let (_guard, _dir, cwd) = temp_env("clear-restores");
        let session = "s-clear";
        let mark_input = hook_input("WebSearch", json!({"query": "x"}), &cwd, session);
        decide_mark(&mark_input).unwrap();
        assert_enforced(&decide_gate_with(
            &hook_input("Bash", json!({"command": "x"}), &cwd, session),
            ENFORCE,
        ));

        state::clear(&cwd, session).unwrap();
        assert_eq!(
            decide_gate_with(
                &hook_input("Bash", json!({"command": "x"}), &cwd, session),
                ENFORCE
            ),
            GateAction::Silent
        );
    }

    #[test]
    fn analyse_gate_panic_barrier_fails_closed() {
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            analyse_gate_with(|_: &HookInput| -> GateAction { panic!("boom") })
        }));
        std::panic::set_hook(prev);
        assert_enforced(&out.unwrap());
    }

    /// Drive the REAL barrier ([`analyse_gate_barrier`]) with a panicking
    /// decision function. No look-alike copy of the barrier lives in the test
    /// module any more, so these tests fail if production's panic arm regresses.
    fn analyse_gate_with<F>(f: F) -> GateAction
    where
        F: FnOnce(&HookInput) -> GateAction,
    {
        let dummy = HookInput::default();
        analyse_gate_barrier(&dummy, f)
    }

    // -----------------------------------------------------------------------
    // observe-only mode
    // -----------------------------------------------------------------------

    /// ANTI-VACUITY (a): observe-only must not change the `Clean` path. A clean
    /// turn stays silent — no warning, no ledger line. Without this, an
    /// implementation that warned on every call would still pass the tests
    /// below.
    #[test]
    fn observe_only_leaves_the_clean_path_silent_and_unrecorded() {
        let (_guard, _dir, cwd) = temp_env("observe-clean");
        let gate_input = hook_input("Bash", json!({"command": "ls"}), &cwd, "s-observe-clean");
        assert_eq!(
            decide_gate_with(&gate_input, observe::Posture::ObserveOnly),
            GateAction::Silent,
            "observe-only must not invent a finding on a clean turn"
        );
        emit_gate(&gate_input, GateAction::Silent);
        assert_eq!(
            observe::tally(&cwd).unwrap(),
            (0, 0),
            "a clean turn must not write a ledger line"
        );
    }

    /// A tainted turn under observe-only emits an `additionalContext` warning
    /// and — critically — **no `permissionDecision` at all**. An explicit
    /// `allow` here would override other gates and the user's own permission
    /// rules, so its absence is part of the contract, not an omission.
    #[test]
    fn observe_only_warns_without_any_permission_decision() {
        let (_guard, _dir, cwd) = temp_env("observe-tainted");
        let session = "s-observe-tainted";
        decide_mark(&hook_input(
            "WebFetch",
            json!({"url": "https://example.com"}),
            &cwd,
            session,
        ))
        .unwrap();

        let gate_input = hook_input("Bash", json!({"command": "rm -rf /"}), &cwd, session);
        let action = decide_gate_with(&gate_input, observe::Posture::ObserveOnly);
        let context = match &action {
            GateAction::Observe { context, .. } => context.clone(),
            other => panic!("expected Observe, got {other:?}"),
        };

        let line = hookio::context_json(&context);
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["hookSpecificOutput"]["hookEventName"], "PreToolUse");
        assert!(
            v["hookSpecificOutput"]["permissionDecision"].is_null(),
            "observe-only must emit NO permissionDecision (an explicit allow would \
             override other gates); got {v}"
        );
        let ctx = v["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .expect("additionalContext must be present and a string");
        // Silence is not an option: the warning must name the taint, the source
        // that caused it, and the fact that enforcement was suppressed.
        assert!(
            ctx.contains("OBSERVE-ONLY"),
            "must announce the posture: {ctx}"
        );
        assert!(ctx.contains("web"), "must name the tainting source: {ctx}");
        assert!(
            ctx.contains("SUPPRESSED"),
            "must say enforcement was suppressed: {ctx}"
        );
        assert!(
            ctx.contains("Bash"),
            "must name the tool that would have been downgraded: {ctx}"
        );
    }

    /// The countable signal: a suppressed enforcement lands one ledger line
    /// carrying the trigger source and the tool.
    #[test]
    fn observe_only_records_the_suppressed_enforcement() {
        let (_guard, _dir, cwd) = temp_env("observe-ledger");
        let session = "s-observe-ledger";
        decide_mark(&hook_input(
            "WebSearch",
            json!({"query": "x"}),
            &cwd,
            session,
        ))
        .unwrap();

        let gate_input = hook_input("Edit", json!({"file_path": "a.rs"}), &cwd, session);
        let action = decide_gate_with(&gate_input, observe::Posture::ObserveOnly);
        emit_gate(&gate_input, action);

        assert_eq!(
            observe::tally(&cwd).unwrap(),
            (1, 0),
            "exactly one parseable ledger line must be appended"
        );
        let text = std::fs::read_to_string(observe::ledger_path(&cwd)).unwrap();
        let rec: observe::Record = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        assert_eq!(rec.tool, "Edit");
        assert_eq!(rec.sources, vec!["web".to_string()]);
        assert_eq!(rec.check, "tainted");
        assert_eq!(rec.session, session);
    }

    /// The ledger accumulates (append-only), so a fire-rate can be totalled.
    #[test]
    fn observe_only_ledger_accumulates_across_calls() {
        let (_guard, _dir, cwd) = temp_env("observe-accum");
        let session = "s-observe-accum";
        decide_mark(&hook_input(
            "WebFetch",
            json!({"url": "https://e.com"}),
            &cwd,
            session,
        ))
        .unwrap();
        for tool in ["Bash", "Write", "Edit"] {
            let gi = hook_input(tool, json!({}), &cwd, session);
            let action = decide_gate_with(&gi, observe::Posture::ObserveOnly);
            emit_gate(&gi, action);
        }
        assert_eq!(observe::tally(&cwd).unwrap(), (3, 0));
    }

    /// An `Undetermined` check under observe-only is recorded as
    /// `"undetermined"`, NOT folded into `"tainted"` — otherwise a store-health
    /// problem would hide inside the friction statistic. Driven by a corrupt
    /// marker, the same fault `state.rs` uses for its own Undetermined test.
    #[test]
    fn observe_only_keeps_undetermined_distinct_from_tainted() {
        let (_guard, _dir, cwd) = temp_env("observe-undet");
        let session = "s-observe-undet";
        let marker = state::marker_path_for_test(&cwd, session);
        std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
        std::fs::write(&marker, b"{ not json").unwrap();

        let gate_input = hook_input("Bash", json!({}), &cwd, session);
        let action = decide_gate_with(&gate_input, observe::Posture::ObserveOnly);
        match &action {
            GateAction::Observe { context, record } => {
                assert_eq!(record.check, "undetermined");
                assert!(record.sources.is_empty());
                assert!(
                    context.contains("could not verify"),
                    "the warning must say the state was unreadable: {context}"
                );
            }
            other => panic!("expected Observe, got {other:?}"),
        }
        // And it must NOT be silent — an unreadable store is not a clean turn.
        assert_ne!(action, GateAction::Silent);
    }

    /// ANTI-VACUITY (b): with the env var absent or set to garbage, the gate
    /// ENFORCES exactly as before. This is the test that makes the fail-closed
    /// opt-in claim non-empty; without it, `resolve` could return `ObserveOnly`
    /// unconditionally and every other observe-only test would still pass.
    #[test]
    fn absent_or_garbage_env_still_enforces() {
        let (_guard, _dir, cwd) = temp_env("observe-failclosed");
        let session = "s-failclosed";
        decide_mark(&hook_input(
            "WebFetch",
            json!({"url": "https://e.com"}),
            &cwd,
            session,
        ))
        .unwrap();
        let gate_input = hook_input("Bash", json!({}), &cwd, session);

        for raw in [
            None,
            Some(""),
            Some("0"),
            Some("false"),
            Some("true"),
            Some("yes"),
            Some(" 1"),
            Some("1 "),
            Some("01"),
            Some("2"),
            Some("OBSERVE"),
        ] {
            let posture = observe::resolve(raw);
            assert_eq!(
                posture,
                observe::Posture::Enforce,
                "{raw:?} must NOT opt into observe-only"
            );
            assert_enforced(&decide_gate_with(&gate_input, posture));
        }
        assert_eq!(
            observe::tally(&cwd).unwrap(),
            (0, 0),
            "enforcing must not write observe-only ledger lines"
        );
    }

    /// A panic fails closed to ask/deny **even though** observe-only is the
    /// posture: cannot-determine resolves to the restricted side regardless of
    /// posture, and the barrier cannot trust a posture it never finished
    /// reading.
    #[test]
    fn panic_enforces_even_under_observe_only() {
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            // `analyse_gate`'s real barrier, driven by a panicking decision fn.
            analyse_gate_with(|_: &HookInput| -> GateAction {
                let _ = observe::Posture::ObserveOnly;
                panic!("boom inside observe-only")
            })
        }));
        std::panic::set_hook(prev);
        assert_enforced(&out.unwrap());
    }
}
