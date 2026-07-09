//! blastguard — a Claude Code PreToolUse hook that denies project-destroying
//! Bash commands and file operations.
//!
//! Contract (shared by every plugin in this repo): a hook must NEVER break the
//! user's turn. We read the tool call from stdin, decide allow/deny with a pure
//! function, and — only on a deny — print the single-line PreToolUse JSON. On
//! empty/invalid input, an unmatched tool, or any panic we stay silent and exit
//! 0. `harness_core::hook::run_hook` enforces the panic half of that invariant.

use blastguard::model::Decision;
use blastguard::{detect, hookio, rule_id};
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
    // never-break-a-turn: swallow panics, always exit 0.
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
        None => return, // empty/invalid stdin → stay silent.
    };
    let decision = detect::detect(&input.tool_name, input.tool_input.as_ref());
    if let Decision::Deny(reason) = decision {
        // Fail-soft fleet violation record for overwatch's cross-task
        // correlated-error detection. This is purely additive telemetry: it
        // must never change the deny decision, the printed JSON, or the exit
        // code, and must not panic if the store is unwritable (e.g. HOME
        // unset, disk full, permissions). Swallow any error.
        record_violation(&input, &reason);
        println!("{}", hookio::deny_json(&reason));
    }
    // Allow → print nothing.
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
    let _ = overwatch::store::append_violation(&cwd, &event);
}
