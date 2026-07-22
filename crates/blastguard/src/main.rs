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
//! normal, non-breaking outcome. Empty/invalid input and an unmatched tool are
//! still a silent allow — those are cases where we successfully determined
//! there is nothing to judge, not cases where we failed to determine anything.

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
    let input = match HookInput::parse(&raw) {
        Some(i) => i,
        None => return, // empty/invalid stdin → nothing to judge → stay silent.
    };

    let decision = analyse(&input);

    // An `Ask` is only a real question when someone can answer it. In a headless
    // or agent-driven session it is not a pause, it is a block the agent cannot
    // clear — so it is hardened to a deny instead. Never to an allow: "ask に
    // できないときは fail".
    let decision = if interactive::ask_available() {
        decision
    } else {
        decision.hardened()
    };

    if let Some(line) = hookio::decision_line(&decision) {
        // Fail-soft fleet violation record for overwatch's cross-task
        // correlated-error detection. Purely additive telemetry: it must never
        // change the decision, the printed JSON, or the exit code, and must not
        // panic if the store is unwritable (HOME unset, disk full, permissions).
        // Asks are recorded too — a recurring ask is the signal that some real
        // construct is going unanalysed.
        if let Decision::Deny(reason) | Decision::Ask(reason) = &decision {
            record_violation(&input, reason);
        }
        println!("{line}");
    }
    // Allow → print nothing.
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
