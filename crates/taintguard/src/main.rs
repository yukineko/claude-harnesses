// テスト内の unwrap/expect/panic は意図的な assert であって fail-open ではないので許可する。
// production 側は workspace の [workspace.lints.clippy] で deny のまま。
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
//! taintguard — a Claude Code hook trio implementing a provenance-scoped
//! least-privilege gate.
//!
//! Contract (shared by every plugin in this repo): a hook must NEVER break the
//! user's turn. The three HOOK subcommands (`mark`/`gate`/`clear`) read a hook
//! payload from stdin and always exit 0 (`harness_core::hook::run_hook`). The
//! fourth subcommand, `tally`, is **not** a hook — see [`run_tally`].
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
//! * `tally` (CLI readout, NOT a hook) — print the observe-only ledger totals
//!   for the project in the process current directory. Reads no stdin, is not
//!   wrapped in `run_hook`, and exits non-zero when the tally could not be read.
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
    /// CLI readout (NOT a hook): print the observe-only ledger totals.
    Tally {
        /// Emit machine-readable JSON instead of human-readable text.
        #[arg(long)]
        json: bool,
    },
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
        // Deliberately NOT `run_hook` — see `run_tally`'s docs.
        Command::Tally { json } => run_tally(json),
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
///
/// The `Observe` arm prints the warning twice over, in two different channels
/// ([`hookio::observe_json`]): once as `additionalContext`, which per the hooks
/// reference is injected into the model's context rather than shown in the
/// interface, and once as a top-level `systemMessage`, documented only as a
/// warning shown to the user. The second is **best-effort**: the docs contain no
/// example pairing `systemMessage` with a PreToolUse response that carries no
/// `permissionDecision`, so its rendering on this shape is undocumented and this
/// code does not rely on it. The readout an operator can actually count on is
/// the ledger line appended just below plus `taintguard tally`.
fn emit_gate(input: &HookInput, action: GateAction) {
    match action {
        GateAction::Silent => {}
        GateAction::Enforce(line) => println!("{line}"),
        GateAction::Observe { context, record } => {
            // The same text in both channels: it already names the posture
            // (`OBSERVE-ONLY`), the suppression (`SUPPRESSED`) and the tool, so
            // a second wording could only drift away from the first.
            println!("{}", hookio::observe_json(&context, &context));
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

// ---------------------------------------------------------------------------
// tally (operator readout — NOT a hook)
// ---------------------------------------------------------------------------

/// Print the observe-only ledger totals for the project in the process current
/// directory: the number of parseable records (`suppressed`) and the number of
/// lines that could not be parsed (`corrupt`), as reported by
/// [`observe::tally`]. Never returns on the failure path (it exits).
///
/// **Why this is not wrapped in `run_hook`.** `harness_core::hook::run_hook`
/// ends in `std::process::exit(0)`. For a hook that is exactly right — a hook
/// must never break the user's turn — and for an operator readout it is exactly
/// wrong: it would make a non-zero exit unreachable, so "I could not read the
/// tally" and "the tally is zero" would leave the shell with the same status.
/// `tally` is therefore called directly from `main`, reads no stdin, and is
/// allowed — required — to exit non-zero. `mark`/`gate`/`clear` keep their
/// `run_hook` wrappers unchanged.
///
/// **Why the failure path prints no count.** A tally that could not be read is
/// cannot-determine, not zero (CLAUDE.md §3). On `Err` — and equally when the
/// process cwd itself cannot be read, since "I do not know which project" is
/// not "this project has zero" — this prints the propagated message to stderr,
/// prints *nothing at all* on stdout, and exits 1. There is deliberately no
/// `unwrap_or` / `unwrap_or_default` / `.ok()` fallback anywhere here: a count
/// that was never read must be unrepresentable in the output, in JSON mode too
/// (the failure object carries `error` and no `suppressed`/`corrupt` key).
///
/// A genuine zero therefore stays distinguishable from a read failure in the
/// OUTPUT as well as via the exit code: `ok == 0 && corrupt == 0` prints an
/// extra `nothing observed yet` line, which the failure path never prints.
fn run_tally(json: bool) {
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        // No fallback to `.` (which would tally whatever project the resolver
        // happened to land on) and no fallback to a zero tally.
        Err(e) => fail_tally(
            json,
            &format!(
                "could not read the process current directory ({e}), so which project to tally \
                 is unknown"
            ),
        ),
    };
    match observe::tally(&cwd) {
        Ok((ok, corrupt)) => {
            let ledger = observe::ledger_path(&cwd);
            if json {
                // `serde_json::json!` so both counts serialize as JSON numbers
                // (a stringified count silently misreads under `jq '.suppressed
                // > 0'`). Exactly the three contract keys, no more.
                println!(
                    "{}",
                    serde_json::json!({
                        "ledger": ledger.to_string_lossy(),
                        "suppressed": ok,
                        "corrupt": corrupt,
                    })
                );
            } else {
                // The two counts are printed on separate lines and never
                // summed: a corrupt tail is a store-health problem, not extra
                // suppressions.
                println!("[taintguard] observe-only ledger: {}", ledger.display());
                println!("  suppressed: {ok}");
                println!("  corrupt: {corrupt}");
                if ok == 0 && corrupt == 0 {
                    println!(
                        "  nothing observed yet (the ledger was read successfully and holds no \
                         records — a real zero, not a failed read)"
                    );
                }
            }
        }
        Err(why) => fail_tally(json, &why),
    }
}

/// Report a tally that could NOT be produced, then exit non-zero. Never
/// returns.
///
/// `why` is the message from the failed step (propagated verbatim, so
/// `observe::tally`'s own `could not read observe-only ledger …` text reaches
/// the operator rather than being replaced by a generic one). The disclaimer is
/// appended because the exit code alone is easy to drop when this is piped: the
/// text itself has to say that no count was produced.
///
/// Nothing is written to stdout on this path — not even a zero — so a caller
/// that only captures stdout cannot mistake a failed read for a clean
/// measurement. In JSON mode stderr carries the error object *alone*, so a
/// consumer can parse stderr directly, and the object has no `suppressed` /
/// `corrupt` key at all.
fn fail_tally(json: bool, why: &str) -> ! {
    let message = format!("[taintguard] {why}. This is NOT a tally of zero: the count is UNKNOWN.");
    if json {
        eprintln!("{}", serde_json::json!({"error": message}));
    } else {
        eprintln!("{message}");
        eprintln!(
            "[taintguard] No count was produced. Do not record this run as an observe-only \
             measurement."
        );
    }
    std::process::exit(1)
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
    /// by default within one binary). Every test that calls [`temp_env`] holds
    /// this for its whole body via the returned guard.
    ///
    /// The two panic-barrier tests do NOT call `temp_env` and therefore do NOT
    /// hold this lock: they inject a closure that panics before any state or
    /// env read happens, so they must not — and do not — depend on the state
    /// dir or on any env var. If a future panic-barrier test needs either, it
    /// has to take `temp_env` like the rest, or it becomes a test whose verdict
    /// is decided by the ambient environment.
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

    /// Three things, and NOT the env var: for every raw value that is not the
    /// exact opt-in string, [`observe::resolve`] maps it to
    /// [`observe::Posture::Enforce`]; `decide_gate_with`'s `Enforce` arm then
    /// downgrades a tainted turn to ask/deny; and `emit_gate`'s `Enforce` arm
    /// appends no observe-only ledger line.
    ///
    /// WHAT THIS TEST DOES NOT DO — stated plainly, because an earlier docstring
    /// claimed it: it does **not** exercise the real environment variable.
    /// It never calls [`observe::posture()`] and never sets
    /// `TAINTGUARD_OBSERVE_ONLY`; it feeds the raw values to `resolve` directly.
    /// So it cannot catch a `posture()` that ignores or mis-reads the env var,
    /// and it is not, on its own, what makes the fail-closed opt-in claim
    /// non-empty. The env var itself is covered end-to-end, through the real
    /// binary, by `the_real_observe_only_env_var_fails_closed_for_every_non_opt_in_value`
    /// in `crates/taintguard/tests/provenance_gate.rs`.
    ///
    /// It is also weaker than it looks in a second way: because the posture
    /// handed to `decide_gate_with` is the one `resolve` just returned (and is
    /// asserted to be `Enforce`), the loop is eleven repetitions of
    /// "`Enforce` + tainted ⇒ ask/deny". That is worth pinning — it is the arm a
    /// regression in `decide_gate_with` would break — but it is a property of
    /// the mapping and the arm, not of the environment.
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
            let action = decide_gate_with(&gate_input, posture);
            assert_enforced(&action);
            // Emit for real, so the tally assertion below is about `emit_gate`'s
            // Enforce arm rather than about nothing. The previous version of
            // this test asserted `tally == (0, 0)` WITHOUT ever calling
            // `emit_gate`, and `decide_gate_with` does no ledger IO by design —
            // so that assertion could not fail for the reason it stated. Do not
            // "restore" it in that shape: drop this `emit_gate` call and the
            // assertion goes vacuous again.
            emit_gate(&gate_input, action);
        }
        assert_eq!(
            observe::tally(&cwd).unwrap(),
            (0, 0),
            "enforcing must not write observe-only ledger lines, not even when \
             the decision is actually emitted"
        );
    }

    /// A caught panic fails closed to ask/deny, and does so **posture-
    /// independently by construction**: [`analyse_gate_barrier`] never reads a
    /// posture at all — the posture read lives *inside* the closure it guards
    /// (`decide_gate` → `observe::posture()`), so a panic on that path cannot
    /// have produced a posture for the barrier to honour. That structural fact
    /// is the safety property worth pinning here, and it is what makes a panic
    /// enforce no matter what `TAINTGUARD_OBSERVE_ONLY` says.
    ///
    /// WHAT THIS TEST DOES NOT DO: it does not exercise the observe-only posture.
    /// It sets no environment variable and hands the barrier no posture, so it is
    /// deliberately identical in reach to
    /// [`analyse_gate_panic_barrier_fails_closed`] above — the two differ only in
    /// which regression a reader would look here for. An earlier version of this
    /// test claimed to cover observe-only via a dead
    /// `let _ = observe::Posture::ObserveOnly;` statement (removed: it bound
    /// nothing, influenced nothing, and the verdict was in fact decided by the
    /// ambient environment of whoever ran `cargo test`).
    ///
    /// Observe-only-posture behaviour is covered end-to-end, with the real env
    /// var set in a child process, by
    /// `undetermined_state_with_the_real_observe_only_env_set_is_reported_not_silent`
    /// in `crates/taintguard/tests/provenance_gate.rs`. That test also records
    /// that the panic arm itself is NOT reachable from outside the process, which
    /// is why it stays pinned here.
    ///
    /// Renamed from `panic_enforces_even_under_observe_only` (the old name is
    /// still quoted in that `provenance_gate.rs` docstring; grep both).
    #[test]
    fn panic_enforces_regardless_of_posture_because_the_barrier_never_reads_it() {
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            // `analyse_gate`'s real barrier, driven by a panicking decision fn.
            analyse_gate_with(|_: &HookInput| -> GateAction {
                panic!("boom before any posture could be read")
            })
        }));
        std::panic::set_hook(prev);
        assert_enforced(&out.unwrap());
    }
}
