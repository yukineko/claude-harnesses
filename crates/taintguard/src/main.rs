// テスト内の unwrap/expect/panic は意図的な assert であって fail-open ではないので許可する。
// production 側は workspace の [workspace.lints.clippy] で deny のまま。
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
//! taintguard — a Claude Code hook trio implementing a provenance-scoped
//! least-privilege gate.
//!
//! Contract (shared by every plugin in this repo): a hook must never break the
//! user's turn *by crashing or by exiting non-zero*. The three HOOK subcommands
//! (`mark`/`gate`/`clear`) read a hook payload from stdin and always exit 0
//! (`harness_core::hook::run_hook`). The fourth subcommand, `tally`, is **not** a
//! hook — see [`run_tally`].
//!
//! **That contract is about the PROCESS, not about the VERDICT, and the two used
//! to be conflated here** (backlog 9a28b98c). `gate` carries a permission
//! decision, and `run_hook`'s terminal `exit(0)` means an empty stdout is not a
//! neutral outcome — Claude Code reads it as "this hook has no objection", i.e. an
//! allow. So for `gate`, "never break the turn" may only ever mean "exit 0 and
//! print the decision"; it must never be read as "when in doubt, print nothing".
//! Every cannot-determine on `gate`'s path therefore resolves to a printed
//! `ask`/`deny` (CLAUDE.md §3): an unreadable taint marker (`Check::Undetermined`),
//! a panic in the analysis (the barrier below), and — the case 9a28b98c names — a
//! NON-EMPTY hook payload that could not be parsed at all
//! ([`gate_on_unparseable_payload`]). An EMPTY stdin remains silent, because that
//! is not a payload this process failed to understand but no payload at all; the
//! split mirrors `blastguard::main::run` and `ctxrot`'s hooks.
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
use taintguard::{classify, hookio, interactive, observe, readonly, state};

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
            match HookInput::parse(&raw) {
                Some(input) => analyse_mark(&input),
                // KNOWN RESIDUAL, stated rather than hidden. `mark` cannot fail
                // closed here the way `gate` does: a fail-closed mark needs the
                // `session_id` this payload failed to yield, and marking the
                // shared `"default"` session bucket instead would plant a taint
                // that the real session's Stop `clear` can never remove — a
                // permanent, silently wrong block. (Since 0.2.0 the marker is
                // keyed by session ALONE, so that bucket is now shared across
                // every project on the machine rather than merely across one —
                // the residual got wider, not narrower, which is the reason this
                // arm still records nothing.) Inventing a lenient side-parser to
                // recover the session id would be re-implementing the parser that
                // just failed. So this arm records nothing, and the honest
                // consequence is that a `gate` later in the same turn may see
                // `Clean`. It is at least LOUD: the diagnostic below is the only
                // signal, so it must not be dropped.
                None if !raw.trim().is_empty() => eprintln!(
                    "[taintguard] mark received a NON-EMPTY hook payload it could not parse \
                     ({} bytes); no taint was recorded, so a later `gate` in this turn may see \
                     a clean session. This is a known limitation of the mark side: the payload \
                     that failed to parse is also what carries the session id the marker would \
                     be keyed by.",
                    raw.len()
                ),
                // Empty stdin: not invoked as a hook at all. Nothing to say.
                None => {}
            }
        }),
        Command::Gate => run_hook(|| {
            let raw = read_stdin();
            match HookInput::parse(&raw) {
                Some(input) => emit_gate(&input, analyse_gate(&input)),
                // A payload arrived and could not be read: cannot-determine on a
                // VERDICT path, so it must be printed as ask/deny, never left to
                // `run_hook`'s exit(0) silent allow. See the module docs.
                None if !raw.trim().is_empty() => gate_on_unparseable_payload(raw.len()),
                None => {}
            }
        }),
        Command::Clear => run_hook(|| {
            let raw = read_stdin();
            match HookInput::parse(&raw) {
                Some(input) => {
                    if let Err(reason) = state::clear(&input.session_id) {
                        eprintln!(
                            "[taintguard] clear failed (staying tainted, the safe side): {reason}"
                        );
                    }
                }
                // Not clearing IS the safe side (the marker stays, so the gate
                // keeps enforcing), so unlike `mark` this needs no fail-closed
                // action — only to be visible.
                None if !raw.trim().is_empty() => eprintln!(
                    "[taintguard] clear received a NON-EMPTY hook payload it could not parse \
                     ({} bytes); no marker was cleared, so this session stays tainted (the safe \
                     side) until a parseable Stop payload arrives.",
                    raw.len()
                ),
                None => {}
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
        "WebFetch" | "WebSearch" => state::mark(session, "web"),
        "Read" => match input.target() {
            // `cwd` still decides WHETHER a read is external (the classifier is
            // inherently relative to the project); it no longer decides WHERE
            // the resulting mark is stored — see `state::state_dir`.
            Some(target) => match classify::classify(&cwd, &target) {
                classify::Trust::Trusted => Ok(()),
                classify::Trust::Untrusted | classify::Trust::Indeterminate => {
                    state::mark(session, "external-read")
                }
            },
            // A Read with no extractable file_path is indeterminate — fail
            // closed the same as an indeterminate path, not a silent no-op.
            None => state::mark(session, "external-read"),
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
            if let Err(reason) = state::mark(&session, "internal-error") {
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

/// Emit `gate`'s fail-closed decision for a NON-EMPTY hook payload that could
/// not be parsed into a [`HookInput`] (backlog 9a28b98c).
///
/// This is a cannot-determine, and on this subcommand it is the *worst* kind:
/// with no `cwd` and no `session_id` there is not even a taint marker to look up,
/// so the process cannot form an opinion about the session at all. Before this
/// existed, `main`'s `if let Some(input)` simply fell through, `run_hook` exited
/// 0 with empty stdout, and Claude Code proceeded with the write-class tool —
/// a silent allow reached by a parse failure, which is precisely the collapse
/// CLAUDE.md §3 forbids.
///
/// The reason deliberately says the payload could not be READ and does **not**
/// claim a taint finding: reporting "this turn consumed untrusted content" would
/// be asserting something this process never determined (CLAUDE.md §4). It also
/// prints the byte count, because "unparseable" plus a length is the only
/// diagnostic available when the payload itself cannot be trusted to echo.
///
/// `build_decision` hardens the `ask` to `deny` when no human is detected — and
/// headless/subagent is the common context for this path, so the hardening is not
/// theoretical.
fn gate_on_unparseable_payload(raw_len: usize) {
    eprintln!(
        "[taintguard] gate received a NON-EMPTY hook payload it could not parse ({raw_len} \
         bytes); failing closed (ask/deny) rather than exiting 0 with no decision, which \
         Claude Code would read as an allow."
    );
    println!(
        "{}",
        build_decision(
            "[taintguard] could not parse this hook's payload, so this session's taint state \
             could not even be looked up (no cwd, no session id); failing closed (treating \
             this tool call as unverified). This is NOT a taint finding — nothing was \
             determined about this turn's provenance."
        )
    );
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
///
/// Narrower than that, though: **only `Check::Tainted` can reach `Observe`.**
/// `Check::Undetermined` resolves to `Enforce` in *both* postures (see
/// [`decide_gate_with`]), so `Observe` means exactly one thing — "a known taint,
/// with named sources, whose enforcement was suppressed for measurement".
#[derive(Debug, Clone, PartialEq, Eq)]
enum GateAction {
    /// The check came back `Clean`. Print nothing at all — byte-identical to
    /// this crate's behaviour before observe-only existed.
    Silent,
    /// A finding, and this process is either in the enforce posture **or** the
    /// finding is a cannot-determine (which enforces in either posture): print
    /// the `ask`/`deny` PreToolUse decision.
    Enforce(String),
    /// A **`Tainted`** finding, and this process is in the observe-only posture:
    /// print an `additionalContext` warning carrying **no**
    /// `permissionDecision`, and append `record` to the ledger so the suppressed
    /// enforcement is counted. Reachable from `Check::Tainted` only — a
    /// cannot-determine is never suppressed, so it never lands here and
    /// therefore never writes a ledger line.
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

/// The `Bash` tool's `command` string, when the payload actually carries one.
///
/// `None` for a missing field, a non-string field, or a payload with no
/// `tool_input` at all — every one of which flows to the taint check rather
/// than to the read-only fast path, because a command that could not be read
/// has not been classified (CLAUDE.md §3).
fn readonly_command(input: &HookInput) -> Option<&str> {
    input.tool_input.as_ref()?.get("command")?.as_str()
}

/// [`decide_gate`] with the posture injected, so tests can drive both postures
/// without mutating the process-global environment.
///
/// The posture is consulted on **exactly one** of the three arms below —
/// `Check::Tainted`. `Check::Clean` is silent in either posture, and
/// `Check::Undetermined` enforces in either posture; see that arm's comment.
fn decide_gate_with(input: &HookInput, posture: observe::Posture) -> GateAction {
    let tool = input.tool_name.as_str();
    // A `Bash` invocation that is statically known to write nothing is not a
    // write-class tool, so the taint state is not consulted for it at all
    // (backlog a4b59893). This is a narrowing of the MATCHER, not of the
    // invariant: the gate's own message has always promised to downgrade
    // "write-class tools", while the hook matched the whole `Bash` tool, which
    // left a tainted turn unable to run `git status` — unable to diagnose
    // itself, and with no route back for a non-interactive worker.
    // `is_readonly_bash` answers `false` for everything it does not positively
    // recognise, so an unrecognised command is gated exactly as before.
    if tool == "Bash" && readonly_command(input).is_some_and(readonly::is_readonly_bash) {
        return GateAction::Silent;
    }
    match state::check(&input.session_id) {
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
        // CANNOT-DETERMINE ENFORCES REGARDLESS OF POSTURE (CLAUDE.md §3).
        //
        // Through 0.1.5 this arm honoured observe-only and returned `Observe`,
        // i.e. "I could not determine the taint state" was silenced by a
        // measurement flag. That is the cannot-determine→permissive collapse §3
        // forbids, and it bought nothing: observe-only exists to measure the
        // friction caused by *known* taint, and an `Undetermined` check names no
        // sources at all (the record's `sources` was always empty), so
        // suppressing it produced no measurement while spending the invariant.
        // "Could not determine" is the same class as "panicked", and the panic
        // barrier below already enforces in either posture — this arm is now
        // consistent with it.
        //
        // NO LEDGER LINE IS WRITTEN HERE, deliberately. The ledger — and
        // `taintguard tally`'s `suppressed` field that reads it — is *defined* as
        // the count of suppressed enforcements (`observe::Record`, `observe::tally`).
        // This event was not suppressed; it enforced. Appending it would inflate a
        // counter whose own name asserts suppression, so the honest options were
        // "don't record" or "change tally's output contract to split `suppressed`
        // three ways"; the first is chosen and the README says so explicitly, so
        // no reader is left thinking the ledger counts every time the gate fired.
        // The event stays observable as the `ask`/`deny` it actually produced,
        // whose reason names the un-honoured posture (below) — the same channel
        // the panic arm uses.
        state::Check::Undetermined(why) => {
            let unhonoured = match posture {
                // Nothing to say: no posture was overridden.
                observe::Posture::Enforce => String::new(),
                // Say it, so an operator who deliberately set observe-only can
                // tell "my posture is being ignored/broken" from "this one path
                // never honours it, on purpose".
                observe::Posture::ObserveOnly => {
                    format!(" {}", observe::undetermined_not_suppressed_note())
                }
            };
            GateAction::Enforce(build_decision(&format!(
                "[taintguard] could not verify this session's taint state ({why}); \
                 failing closed (treating this turn as tainted).{unhonoured}"
            )))
        }
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
/// of posture (CLAUDE.md §3). It is loud in stderr besides.
///
/// This used to be described as "the one case where observe-only does not
/// suppress". It no longer is: `Check::Undetermined` also enforces in either
/// posture (see [`decide_gate_with`]'s comment on that arm). Observe-only now
/// suppresses exactly one thing — a `Check::Tainted` finding — which is the only
/// thing it could ever have measured.
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
                    // Deliberately does NOT say "the ledger was read
                    // successfully": this branch is also reached when the file
                    // is ABSENT, and `harness_core::boundary::read_to_string`
                    // maps every `NotFound` to `Known(None)` — including the
                    // `ENOENT` of a DANGLING SYMLINK at the ledger path, where
                    // nothing was opened at all. An earlier wording claimed a
                    // successful read and was false for that input. What this
                    // line can honestly assert is the part that matters to the
                    // reader: no read FAILED, so this zero is a real zero and
                    // not a swallowed error — the failure path never prints a
                    // count and always exits non-zero.
                    println!(
                        "  nothing observed yet (no records, and no read error — a real zero. \
                         The ledger is absent or empty; a read that FAILED would have exited \
                         non-zero and printed no count at all.)"
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
    /// It also guards `TAINTGUARD_OBSERVE_ONLY`, for the same reason: one lock,
    /// not one per variable, because two independent locks would not exclude each
    /// other.
    ///
    /// [`analyse_gate_panic_barrier_fails_closed`] does NOT call [`temp_env`] and
    /// does NOT hold this lock: it injects a closure that panics before any state
    /// or env read happens, so it must not — and does not — depend on the state dir
    /// or on any env var.
    ///
    /// [`panic_enforces_under_both_real_postures`] is the deliberate exception. It
    /// takes this lock DIRECTLY (not via `temp_env`, since it needs no state dir)
    /// because it sets `TAINTGUARD_OBSERVE_ONLY` for real — that is the whole point
    /// of it, and the reason it can assert posture-independence instead of merely
    /// documenting it. Any other panic-barrier test that needs the state dir or an
    /// env var must likewise take the lock, or its verdict is decided by the
    /// ambient environment of whoever ran `cargo test`.
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

        // `touch out.txt`, NOT the `echo hi` this test used through 0.1.10.
        // `echo` is on `readonly`'s allowlist as of 0.2.0, so `echo hi` now
        // short-circuits to `Silent` BEFORE the taint state is consulted —
        // this test would have been asserting the allowlist, not the taint
        // gate. The assertion is unchanged; only the stand-in for "an
        // arbitrary write-class Bash command" is.
        let gate_input = hook_input("Bash", json!({"command": "touch out.txt"}), &cwd, session);
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

        state::clear(session).unwrap();
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
        // `touch out.txt`, NOT the `ls` this test used through 0.1.10: `ls` is
        // on `readonly`'s allowlist as of 0.2.0, so it would reach `Silent` via
        // the read-only fast path without ever consulting the taint state —
        // making an ANTI-VACUITY test vacuous, which is the one thing it may
        // not be. The assertion is unchanged.
        let gate_input = hook_input(
            "Bash",
            json!({"command": "touch out.txt"}),
            &cwd,
            "s-observe-clean",
        );
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
    ///
    /// This serializes through [`hookio::observe_json`] — the function
    /// `emit_gate` actually calls. It used to call `hookio::context_json`, which
    /// after the `systemMessage` change had ZERO production callers, so this
    /// test pinned a shape nothing emitted while its docstring claimed to pin
    /// observe-only's real output. That is the same "all call sites are
    /// `#[cfg(test)]`" defect this release fixes for `observe::tally`, so it is
    /// fixed here too rather than left as an irony. The end-to-end shape
    /// (including the top-level `systemMessage`) is pinned against the real
    /// binary by `observe_only_suppression_is_visible_on_stdout_with_a_top_level_system_message`
    /// in `tests/provenance_gate.rs`.
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

        let line = hookio::observe_json(&context, &context);
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["hookSpecificOutput"]["hookEventName"], "PreToolUse");
        // The top-level `systemMessage` is a sibling of `hookSpecificOutput`,
        // never nested inside it (best-effort channel; see `observe_json`).
        assert!(
            v["systemMessage"].is_string(),
            "observe-only must carry a TOP-LEVEL systemMessage; got {v}"
        );
        assert!(
            v["hookSpecificOutput"]["systemMessage"].is_null(),
            "systemMessage must NOT be nested inside hookSpecificOutput; got {v}"
        );
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

    /// An `Undetermined` check **enforces in BOTH postures** — observe-only does
    /// NOT suppress a cannot-determine (CLAUDE.md §3: 判定不能は必ず制限側に
    /// 解決する). Driven by a corrupt marker, the same fault `state.rs` uses for
    /// its own Undetermined test.
    ///
    /// This test replaces `observe_only_keeps_undetermined_distinct_from_tainted`,
    /// which asserted the opposite (`GateAction::Observe` with
    /// `record.check == "undetermined"`). That shape only made sense while
    /// observe-only was allowed to swallow a cannot-determine; the human decision
    /// recorded in this change is that it is not. The "keep `undetermined`
    /// distinct from `tainted` in the ledger" property it pinned is now moot on
    /// this path, because **nothing is written to the ledger at all** for an
    /// enforced `Undetermined` — asserted below.
    ///
    /// ## Why no ledger line
    ///
    /// The ledger and `taintguard tally`'s `suppressed` counter are defined as
    /// *suppressed enforcements*. This event was NOT suppressed — it enforced —
    /// so recording it would inflate a counter whose name asserts suppression.
    /// The absence is deliberate, not an oversight, which is why it has an
    /// assertion rather than a comment.
    ///
    /// ## WHAT THIS TEST DOES NOT DO
    ///
    /// It injects the posture directly and therefore never calls
    /// [`observe::posture()`], the only reader of `TAINTGUARD_OBSERVE_ONLY`. So it
    /// cannot catch a `posture()` that mis-reads the env var — that is covered
    /// end-to-end through the real binary by
    /// `observe_only_must_not_suppress_a_cannot_determine_corrupt_marker` in
    /// `crates/taintguard/tests/provenance_gate.rs`. This test pins only the
    /// posture-INDEPENDENCE of the `Undetermined` arm of `decide_gate_with`.
    #[test]
    fn undetermined_enforces_under_both_postures_and_records_no_ledger_line() {
        let (_guard, _dir, cwd) = temp_env("observe-undet");
        let session = "s-observe-undet";
        let marker = state::marker_path_for_test(session);
        std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
        std::fs::write(&marker, b"{ not json").unwrap();

        let gate_input = hook_input("Bash", json!({}), &cwd, session);

        for posture in [observe::Posture::ObserveOnly, observe::Posture::Enforce] {
            let action = decide_gate_with(&gate_input, posture);
            assert!(
                matches!(action, GateAction::Enforce(_)),
                "an unreadable taint marker must ENFORCE under {posture:?} — observe-only must \
                 not suppress a cannot-determine, got {action:?}"
            );
            assert_enforced(&action);
            // Not silent, and not the suppressed-observe shape either: an
            // unreadable store is neither a clean turn nor a measured one.
            assert_ne!(action, GateAction::Silent);

            // And emitting it appends nothing to the observe-only ledger, in
            // either posture: this enforcement was not suppressed, so the
            // `suppressed` counter must not move.
            emit_gate(&gate_input, action);
            assert_eq!(
                observe::tally(&cwd).unwrap(),
                (0, 0),
                "an ENFORCED cannot-determine must append no observe-only ledger line under \
                 {posture:?} — the ledger counts suppressed enforcements, and this one was not \
                 suppressed"
            );
        }
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

    /// Set `TAINTGUARD_OBSERVE_ONLY` to `value` (or remove it for `None`) for the
    /// life of the returned guard, restoring the previous value on drop so a
    /// panicking assertion cannot leak an observe-only posture into later tests.
    ///
    /// The caller must already hold [`ENV_LOCK`]: this mutates process-global
    /// state.
    struct ObserveOnlyEnv(Option<String>);

    impl ObserveOnlyEnv {
        fn set(value: Option<&str>) -> Self {
            let previous = std::env::var(observe::OBSERVE_ONLY_ENV).ok();
            match value {
                Some(v) => std::env::set_var(observe::OBSERVE_ONLY_ENV, v),
                None => std::env::remove_var(observe::OBSERVE_ONLY_ENV),
            }
            ObserveOnlyEnv(previous)
        }
    }

    impl Drop for ObserveOnlyEnv {
        fn drop(&mut self) {
            match &self.0 {
                Some(v) => std::env::set_var(observe::OBSERVE_ONLY_ENV, v),
                None => std::env::remove_var(observe::OBSERVE_ONLY_ENV),
            }
        }
    }

    /// A caught panic fails closed to ask/deny **under the real observe-only
    /// posture as well as the real enforce posture** (backlog 2708deac).
    ///
    /// ## What was empty about the previous versions
    ///
    /// The original `panic_enforces_even_under_observe_only` "covered" observe-only
    /// with a dead `let _ = observe::Posture::ObserveOnly;` statement — it bound
    /// nothing and influenced nothing, so the verdict was decided by whatever
    /// `TAINTGUARD_OBSERVE_ONLY` happened to be in the shell of whoever ran
    /// `cargo test`. The replacement dropped the dead line and honestly documented
    /// that it therefore did *not* exercise the posture at all — correct prose, but
    /// it left the name's claim untested: nothing in the suite established that a
    /// panic enforces when the process really is in observe-only.
    ///
    /// ## What makes this one effective
    ///
    /// It drives [`analyse_gate_barrier`] — the production barrier — once per REAL
    /// posture, with `TAINTGUARD_OBSERVE_ONLY` actually set in this process, and it
    /// asserts via [`observe::posture`] that the posture it claims to be testing is
    /// the one in force. Without that positive control the loop would be two
    /// identical iterations and could pass even if the env var were never applied.
    ///
    /// So it now fails against three distinct regressions, where the previous
    /// version failed against only the first:
    ///
    /// 1. the barrier's `Err` arm returning `Silent` (or dropping the decision);
    /// 2. the barrier growing a posture read and honouring it — returning
    ///    `Observe` under observe-only, which `assert_enforced` rejects;
    /// 3. `posture()` ceasing to reflect the env var (the control fails).
    ///
    /// It holds [`ENV_LOCK`] because it mutates a process-global env var — the one
    /// exception to the note on that lock's docs about the panic-barrier tests not
    /// needing it, and stated here so the two are not read as contradictory.
    #[test]
    fn panic_enforces_under_both_real_postures() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        for (raw, expected) in [
            (None, observe::Posture::Enforce),
            (
                Some(observe::OBSERVE_ONLY_OPT_IN),
                observe::Posture::ObserveOnly,
            ),
        ] {
            let _env = ObserveOnlyEnv::set(raw);
            // POSITIVE CONTROL: prove the process really is in the posture this
            // iteration claims to test. Without it the loop is vacuously two
            // copies of the same run.
            assert_eq!(
                observe::posture(),
                expected,
                "the env fixture did not take effect: with {raw:?} the process must be in \
                 {expected:?}"
            );

            let prev = std::panic::take_hook();
            std::panic::set_hook(Box::new(|_| {}));
            let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                // `analyse_gate`'s real barrier, driven by a panicking decision fn.
                analyse_gate_with(|_: &HookInput| -> GateAction {
                    panic!("boom before any posture could be read")
                })
            }));
            std::panic::set_hook(prev);

            let action = out.unwrap();
            assert_enforced(&action);
            assert!(
                matches!(action, GateAction::Enforce(_)),
                "a panic must resolve to Enforce even when the process is in {expected:?} — \
                 a panic means the analysis never completed, so neither the taint state nor \
                 the posture it was supposed to honour was determined; got {action:?}"
            );
        }
    }

    // -----------------------------------------------------------------------
    // read-only Bash allowlist at the GATE (backlog a4b59893)
    // -----------------------------------------------------------------------

    /// Taint `session` via a real `WebFetch` mark and prove the marker landed,
    /// so every "…is Silent" assertion below is known to be about the read-only
    /// fast path rather than about a session that was never tainted.
    fn taint_and_confirm(cwd: &std::path::Path, session: &str) {
        decide_mark(&hook_input(
            "WebFetch",
            json!({"url": "https://example.com"}),
            cwd,
            session,
        ))
        .unwrap();
        assert!(
            state::is_tainted(session),
            "fixture precondition: {session} must actually be tainted before these assertions"
        );
    }

    /// A tainted turn must still be able to DIAGNOSE itself: a Bash command
    /// statically known to write nothing is not a write-class tool, so the gate
    /// stays silent for it.
    #[test]
    fn read_only_bash_is_silent_even_when_the_session_is_tainted() {
        let (_guard, _dir, cwd) = temp_env("readonly-silent");
        let session = "s-readonly-silent";
        taint_and_confirm(&cwd, session);

        for command in [
            "git status",
            "git log --oneline",
            "git diff",
            "git worktree list",
            "ls -la",
            "pwd",
            "git status | wc -l",
        ] {
            let gate_input = hook_input("Bash", json!({ "command": command }), &cwd, session);
            assert_eq!(
                decide_gate_with(&gate_input, ENFORCE),
                GateAction::Silent,
                "a tainted turn must still be able to run {command:?}"
            );
        }
    }

    /// ANTI-VACUITY for the test above, and the load-bearing half of this pair:
    /// the SAME tainted session, in the SAME fixture, still enforces for
    /// everything that is not on the allowlist. Without this, an
    /// `is_readonly_bash` that returned `true` for every input — or a gate that
    /// had simply stopped checking taint at all — would pass the test above.
    #[test]
    fn the_same_tainted_session_still_enforces_for_write_class_tools() {
        let (_guard, _dir, cwd) = temp_env("readonly-antivacuity");
        let session = "s-readonly-antivacuity";
        taint_and_confirm(&cwd, session);

        // Write-class Bash: not on the allowlist, so the taint state decides.
        for command in [
            "touch out.txt",
            "rm -rf x",
            "git checkout main",
            "cargo test",
        ] {
            let gate_input = hook_input("Bash", json!({ "command": command }), &cwd, session);
            assert_enforced(&decide_gate_with(&gate_input, ENFORCE));
        }

        // Non-Bash write-class tools: the fast path is keyed on the tool being
        // `Bash`, so a `command` field on another tool must not borrow it.
        assert_enforced(&decide_gate_with(
            &hook_input("Write", json!({"file_path": "a.rs"}), &cwd, session),
            ENFORCE,
        ));
        assert_enforced(&decide_gate_with(
            &hook_input("Edit", json!({"file_path": "a.rs"}), &cwd, session),
            ENFORCE,
        ));
        assert_enforced(&decide_gate_with(
            &hook_input("Write", json!({"command": "git status"}), &cwd, session),
            ENFORCE,
        ));
    }

    /// FAIL-CLOSED (CLAUDE.md §3): a `Bash` payload whose `command` could not be
    /// READ is not a command that was classified as read-only. Every shape that
    /// yields no `&str` — absent field, wrong type, `null`, no `tool_input` at
    /// all — must flow to the taint check and enforce, never to the fast path.
    #[test]
    fn bash_with_an_unreadable_command_enforces_when_tainted() {
        let (_guard, _dir, cwd) = temp_env("readonly-unreadable");
        let session = "s-readonly-unreadable";
        taint_and_confirm(&cwd, session);

        for tool_input in [
            json!({}),
            json!({"command": 42}),
            json!({"command": null}),
            json!({"command": ["git", "status"]}),
            json!({"command": {"argv": "git status"}}),
            json!({"cmd": "git status"}),
        ] {
            let gate_input = hook_input("Bash", tool_input.clone(), &cwd, session);
            assert_enforced(&decide_gate_with(&gate_input, ENFORCE));
        }

        // No `tool_input` key at all — not reachable through `hook_input`.
        let no_tool_input = HookInput {
            tool_name: "Bash".to_string(),
            tool_input: None,
            cwd: cwd.to_string_lossy().into_owned(),
            session_id: session.to_string(),
            ..Default::default()
        };
        assert_enforced(&decide_gate_with(&no_tool_input, ENFORCE));
    }

    /// The read-only fast path must not become a way to launder a
    /// cannot-determine: with an UNREADABLE marker the session's taint state is
    /// `Undetermined`, and a read-only command is still silent — because the
    /// fast path never consults the marker at all. This test exists to make
    /// that ordering EXPLICIT rather than incidental, since it is the one place
    /// where `Silent` is reached without a `Check::Clean`.
    ///
    /// The control is in the same body: with the same corrupt marker, a
    /// write-class command enforces, so the silence above is attributable to the
    /// allowlist and not to the marker having been repaired or ignored globally.
    #[test]
    fn the_fast_path_precedes_the_taint_check_and_a_corrupt_marker_still_enforces_write_class() {
        let (_guard, _dir, cwd) = temp_env("readonly-corrupt");
        let session = "s-readonly-corrupt";
        let marker = state::marker_path_for_test(session);
        std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
        std::fs::write(&marker, b"{ not json").unwrap();
        assert!(
            matches!(state::check(session), state::Check::Undetermined(_)),
            "fixture precondition: the marker must be unreadable"
        );

        assert_eq!(
            decide_gate_with(
                &hook_input("Bash", json!({"command": "git status"}), &cwd, session),
                ENFORCE
            ),
            GateAction::Silent,
            "the read-only fast path is evaluated before the marker is read"
        );
        assert_enforced(&decide_gate_with(
            &hook_input("Bash", json!({"command": "touch out.txt"}), &cwd, session),
            ENFORCE,
        ));
    }

    // -----------------------------------------------------------------------
    // session-scoped marker: the cwd dimension is gone (backlog 90d1ca1d)
    // -----------------------------------------------------------------------

    /// A mark made while the payload's `cwd` was A must be visible to a gate
    /// whose payload `cwd` is B. Through 0.1.10 the marker path carried a
    /// `project_key(cwd)` component, so a `cd` between the marking `Read` and
    /// the gating tool call moved the lookup into a different bucket and the
    /// gate answered `Clean` — a silent allow.
    ///
    /// This is the in-process half of the proof. The end-to-end half — where
    /// `mark` and `gate` are separate PROCESSES with different working
    /// directories, which is the only shape that can catch a regression keyed on
    /// `current_dir()` rather than on the payload — is
    /// `crates/taintguard/tests/session_scoped_marker.rs`.
    #[test]
    fn a_mark_under_one_cwd_is_seen_by_a_gate_under_another_cwd() {
        let (_guard, dir, cwd_a) = temp_env("cross-cwd");
        let cwd_b = dir.path().join("other-project");
        std::fs::create_dir_all(&cwd_b).unwrap();
        let session = "s-cross-cwd";

        // Mark from A, via a `Read` of a file outside A — so `cwd` genuinely
        // participates in the classification, exactly as in the real bug.
        let outside = tempfile::Builder::new()
            .prefix("taintguard-cross-cwd-external-")
            .tempdir()
            .unwrap();
        let secret = outside.path().join("secret.txt");
        std::fs::write(&secret, "s").unwrap();
        decide_mark(&hook_input(
            "Read",
            json!({"file_path": secret.to_string_lossy()}),
            &cwd_a,
            session,
        ))
        .unwrap();

        // Gate from B — a directory outside A.
        assert!(
            !cwd_b.starts_with(&cwd_a),
            "fixture precondition: B must not be inside A"
        );
        assert_enforced(&decide_gate_with(
            &hook_input("Bash", json!({"command": "touch out.txt"}), &cwd_b, session),
            ENFORCE,
        ));

        // Control: the same gate from A enforces too, so the assertion above is
        // not passing because B happens to be a special case.
        assert_enforced(&decide_gate_with(
            &hook_input("Bash", json!({"command": "touch out.txt"}), &cwd_a, session),
            ENFORCE,
        ));
    }

    /// ANTI-VACUITY for the test above: the marker is keyed by SESSION, so a
    /// DIFFERENT session id in the same store is still clean. Without this, an
    /// implementation that reported every session tainted would pass.
    #[test]
    fn a_different_session_in_the_same_store_is_still_clean() {
        let (_guard, dir, cwd_a) = temp_env("cross-cwd-control");
        let cwd_b = dir.path().join("other-project");
        std::fs::create_dir_all(&cwd_b).unwrap();

        decide_mark(&hook_input(
            "WebFetch",
            json!({"url": "https://example.com"}),
            &cwd_a,
            "s-marked",
        ))
        .unwrap();

        for cwd in [&cwd_a, &cwd_b] {
            assert_eq!(
                decide_gate_with(
                    &hook_input(
                        "Bash",
                        json!({"command": "touch out.txt"}),
                        cwd,
                        "s-unmarked"
                    ),
                    ENFORCE
                ),
                GateAction::Silent,
                "an unmarked session must stay clean regardless of cwd"
            );
        }
    }
}
