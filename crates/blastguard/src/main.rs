// テスト内の unwrap/expect は意図的な assert であって fail-open ではないので許可する。
// production 側は workspace の [workspace.lints.clippy] で deny のまま。
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
//! blastguard — a Claude Code PreToolUse hook that denies project-destroying
//! Bash commands and file operations.
//!
//! Contract (shared by every plugin in this repo): a hook must NEVER break the
//! user's turn. We read the tool call from stdin, decide allow/deny/ask with a
//! pure function, and — on anything but an allow — print the single-line
//! PreToolUse JSON. We always exit 0.
//!
//! Two things that look alike but are NOT the same, and whose conflation was a
//! defect here:
//!
//!   * "never break the turn" = never crash the session — REQUIRED, and kept.
//!   * "never block the command" = allow even when undecided — NOT required,
//!     and was the bug.
//!
//! A silent exit 0 with no output IS an allow. So the previous contract ("on
//! any panic we stay silent and exit 0") meant that a panic anywhere in the
//! analyser silently ALLOWED the very command it had failed to analyse.
//! `#![deny(clippy::panic)]` does not prevent that: it stops explicit `panic!`
//! only, not index-out-of-bounds, non-char-boundary string slicing, arithmetic
//! overflow or unwrap-on-None. So the analysis now runs inside
//! `std::panic::catch_unwind` and a caught panic becomes a DENY, which is a
//! normal, non-breaking outcome.
//!
//! # What is still a silent allow, and what stopped being one
//!
//! This paragraph used to read "Empty/invalid input and an unmatched tool are
//! still a silent allow — those are cases where we successfully determined
//! there is nothing to judge, not cases where we failed to determine
//! anything." Two of those three were, the middle one was NOT, and the
//! sentence's own criterion is what convicts it:
//!
//!   * EMPTY stdin — nothing to judge. Determined. Silence is accurate.
//!   * an UNMATCHED TOOL (`Read`, `Grep`, …) — outside blastguard's
//!     jurisdiction. Determined. Silence is accurate, and folding it in would
//!     make the hook prompt on every file read.
//!   * INVALID input — a tool call IS being made and blastguard could not read
//!     it. That is the definition of failing to determine, filed under
//!     "successfully determined there is nothing to judge". It allowed
//!     precisely the call it had failed to read.
//!
//! So an unreadable payload now emits `Ask` (hardened to `Deny` where no human
//! can answer) rather than exiting silently, and the same applies one level in:
//! a MATCHED tool whose operand is missing or not a string is a refusal, not an
//! allow. See `detect::unreadable_operand`.
//!
//! The docstring outlived the behaviour it described by describing the
//! behaviour someone intended. CLAUDE.md 4 calls that a trap for the next
//! reviewer, and it worked as one: the analysis side of this crate had applied
//! the three-answer rule to 25-odd sub-analysers while its own front door
//! returned two, and this paragraph is why that read as deliberate.

use blastguard::model::Decision;
use blastguard::rule_id::INTERNAL_ERROR_REASON;
use blastguard::{detect, hookio, interactive, rule_id};
use harness_core::hook::{self, HookInput};
use std::process::exit;

fn main() {
    // Minimal CLI surface: version/help short-circuit before touching stdin.
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--version" | "-V" => {
                println!("blastguard {}", env!("CARGO_PKG_VERSION"));
                exit(0);
            }
            "--help" | "-h" => {
                print_help();
                exit(0);
            }
            _ => {}
        }
    }
    // never-break-a-turn: always exit 0. Panics inside the ANALYSIS are caught
    // by `run` itself and turned into a deny; `run_hook`'s own catch remains the
    // outer backstop for anything outside that scope (stdin read, JSON print).
    hook::run_hook(run);
}

fn print_help() {
    println!(
        "blastguard {ver}\n\
A Claude Code PreToolUse hook that denies project-destroying operations.\n\n\
USAGE:\n  blastguard            read a hook payload from stdin (normal mode)\n  blastguard --version  print version\n  blastguard --help     this help\n\n\
It denies recursive/wildcard rm, git reset --hard, git clean -fdx, truncate,\n\
shred, mkfs, dd of=, recursive chmod/chown, find -delete, and single-> file\n\
overwrites — while exempting repo config files (.claude/**, *.toml, *.lock, …).",
        ver = env!("CARGO_PKG_VERSION")
    );
}

fn run() {
    let raw = hook::read_stdin();

    // EMPTY stdin is the case that genuinely determined there is nothing to
    // judge: no tool call was described, so silence is an accurate answer.
    if raw.trim().is_empty() {
        return;
    }

    let input = match HookInput::parse(&raw) {
        Some(i) => i,
        None => {
            // NON-EMPTY and unparseable is the opposite situation: a tool call
            // is being made and blastguard could not read it. Silence here IS
            // an allow (see the module docstring), so it would allow precisely
            // the call it failed to read. `HookInput::parse` erases the reason
            // via `.ok()`, so the two cases are indistinguishable downstream —
            // which is why they are separated HERE, at the only point that
            // still has the raw bytes.
            emit(Decision::ask(UNREADABLE_PAYLOAD), None);
            return;
        }
    };

    let decision = analyse(&input);

    emit(decision, Some(&input));
}

/// The reason attached to a payload blastguard could not parse at all.
///
/// A `const` rather than an inline literal so `rule_id` classifies it from the
/// same string the emitter uses — the drift between those two would be silent,
/// filing every schema break under `"unknown"`.
const UNREADABLE_PAYLOAD: &str =
    "blastguard could not parse this hook payload, so the tool call was never \
analysed — refusing to guess. If this recurs, the hook's payload schema has \
drifted from what Claude Code sends and needs updating.";

/// Harden, print, and only then record telemetry.
///
/// # The order is the invariant
///
/// `record_violation` used to run BEFORE the `println!`. Its own comment
/// promised the telemetry "must never change the decision, the printed JSON, or
/// the exit code" — but `let _ =` only neutralises the store write RETURNING an
/// error, not any of the event-construction steps PANICKING. A panic there
/// unwinds past this function into `hook::run_hook`, which logs and exits 0
/// with nothing on stdout, and a silent exit 0 IS an allow. So a crash in
/// purely additive telemetry could suppress a deny it was merely observing.
///
/// I could not find a panic reachable in that path today — `rule_id` is
/// `contains` comparisons on `&str`, `store::now` is `unwrap_or(0)`,
/// `cwd_or_current` is `unwrap_or_else`, and neither `build_event` nor
/// `normalize_signature` slices or unwraps. So this is a LATENT fail-open, not
/// a live one, and it is recorded as such rather than dressed up as an
/// exploitable bug. It is still worth closing: the decision should not be
/// hostage to work that has no say in it, and the next line added to that path
/// should not be able to re-open the hole silently. Printing first makes the
/// ordering enforce that, and the inner `catch_and_log` means telemetry can
/// only ever lose ITSELF.
fn emit(decision: Decision, input: Option<&HookInput>) {
    // An `Ask` is only a real question when someone can answer it. In a headless
    // or agent-driven session it is not a pause, it is a block the agent cannot
    // clear — so it is hardened to a deny instead. Never to an allow: "ask に
    // できないときは fail".
    let decision = if interactive::ask_available() {
        decision
    } else {
        decision.hardened()
    };

    let Some(line) = hookio::decision_line(&decision) else {
        return; // Allow → print nothing.
    };

    // The decision reaches Claude Code FIRST. Nothing below can retract it.
    println!("{line}");

    // Fail-soft fleet violation record for overwatch's cross-task
    // correlated-error detection. Asks are recorded too — a recurring ask is
    // the signal that some real construct is going unanalysed.
    //
    // A payload we could not parse has no `HookInput` to attribute to, so it is
    // simply not recorded; inventing a task key for it would pollute the
    // recurrence signature with a synthetic one.
    if let (Some(input), Decision::Deny(reason) | Decision::Ask(reason)) = (input, &decision) {
        let _ = hook::catch_and_log("blastguard-violation-record", || {
            record_violation(input, reason)
        });
    }
}

/// Run the detector with a panic barrier.
///
/// A panic here used to be swallowed into a silent exit 0, and a silent exit 0
/// IS an allow — so any crash in the analyser allowed the command it had just
/// failed to analyse. Catching it and returning a deny keeps the never-break-
/// the-turn contract (a deny is a normal outcome, not a broken turn) while
/// removing the fail-open.
///
/// `catch_unwind` is sound here because the closure borrows only `input` and the
/// detector holds no cross-call mutable state that a half-finished analysis
/// could leave inconsistent — the per-command analysis budget is re-seeded at
/// entry to `detect`, not carried between calls.
fn analyse(input: &HookInput) -> Decision {
    let tool = input.tool_name.clone();
    let tool_input = input.tool_input.clone();
    let result = std::panic::catch_unwind(move || detect::detect(&tool, tool_input.as_ref()));
    match result {
        Ok(decision) => decision,
        Err(_) => Decision::deny(INTERNAL_ERROR_REASON),
    }
}

/// Best-effort: append a `ViolationEvent` for this denial to overwatch's
/// project-scoped violations.jsonl. `reason` is free text (may embed a
/// variable path/target) — `rule_id::rule_id` normalizes it to a stable
/// discriminator before it becomes the violation signature.
fn record_violation(input: &HookInput, reason: &str) {
    let target = input.target();
    let task_key = match &target {
        Some(t) => format!("{}:{}", input.tool_name, t),
        None => input.tool_name.clone(),
    };
    let raw = overwatch::violation::RawViolation {
        rule_id: Some(rule_id::rule_id(reason)),
        ..Default::default()
    };
    let event = overwatch::violation::build_event(
        overwatch::violation::ViolationSource::Blastguard,
        &raw,
        task_key,
        input.session_key(),
        overwatch::store::now(),
        Some(reason.to_string()),
    );
    let cwd = input.cwd_or_current();
    if let Some(event) = event {
        let _ = overwatch::store::append_violation(&cwd, &event);
    }
}
