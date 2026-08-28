//! parallelguard — a per-session concurrency cap on shell calls and subagents,
//! enforced by a binary rather than by prose.
//!
//! Freezing this machine takes one thing: too many processes at once. Every
//! fan-out in this harness (a skill spawning one auditor per shard, a batch of
//! parallel `Bash` calls in a single message) spends the same budget, and until
//! now the only thing standing between a wide fan-out and a frozen WSL2 session
//! was a sentence in a SKILL.md asking the model to please send at most N. That
//! is "気をつける" as a control, which CLAUDE.md 6 says not to build.
//!
//! The gate is a `PreToolUse` hook. It counts what is actually in flight for
//! this session, and refuses the call that would exceed the cap:
//!
//! ```text
//!   PreToolUse(Bash|Task|Agent)   -> acquire  : take a slot, or deny
//!   PostToolUse(Bash|Task|Agent)  -> release  : give the slot back
//!   SessionStart/UserPromptSubmit/Stop -> reset : clear the ledger at a turn boundary
//! ```
//!
//! **Cannot-determine denies.** An unparseable payload, an unreadable ledger, a
//! lock that will not come free, a write that fails, a panic in this binary —
//! each one means the number in flight is UNKNOWN, and an unknown count is not
//! a free slot. Every one of them resolves to `deny` (CLAUDE.md 3). Silence is
//! not available as a degraded mode: a `PreToolUse` hook that exits 0 without
//! output IS an allow, byte for byte, so "the gate broke" would be
//! indistinguishable from "the gate checked and found room".
//!
//! The cost of that choice is bounded on purpose. Every deny is recoverable
//! without a human: the ledger is cleared at every turn boundary by `reset`, an
//! abandoned lockfile is stolen after 30 s, and a denied call is a call that
//! simply did not run — the model re-issues it. Nothing is lost but a round.
//!
//! **What this does NOT bound**, stated so a quiet gap is not mistaken for
//! coverage: a `Bash` call made with `run_in_background: true` returns
//! immediately, so its `PostToolUse` fires while the process keeps running and
//! its slot is released early. Background shells are outside the count.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

mod model;
mod store;

use std::panic::AssertUnwindSafe;

use harness_core::hook::{self, HookInput};
use harness_core::verdict::Determination;

use model::{Decision, SlotClass};

/// Max bytes of the diagnostics log before it is rotated away. The log exists
/// so `status` can show that this gate is alive and what it refused; it is not
/// an archive.
const MAX_EVENT_LOG_BYTES: u64 = 256 * 1024;

fn main() {
    let cmd = std::env::args().nth(1).unwrap_or_default();
    match cmd.as_str() {
        "acquire" => {
            cmd_acquire();
            std::process::exit(0);
        }
        "release" => {
            let _ = std::panic::catch_unwind(AssertUnwindSafe(cmd_release));
            std::process::exit(0);
        }
        "reset" => {
            let _ = std::panic::catch_unwind(AssertUnwindSafe(cmd_reset));
            std::process::exit(0);
        }
        "status" => {
            print!("{}", render_status());
            std::process::exit(0);
        }
        other => {
            eprintln!(
                "parallelguard: unknown command {other:?}\n\
                 usage: parallelguard <acquire|release|reset|status>\n\
                 acquire/release/reset read a Claude Code hook payload on stdin."
            );
            std::process::exit(1);
        }
    }
}

// ---------------------------------------------------------------- acquire

/// PreToolUse entry point: decide, then emit exactly one decision.
///
/// The panic barrier wraps only the decision, never the emit, so a crash
/// produces one `deny` line and not a second one after a decision was already
/// printed. A panic here is a gate that crashed before it could count, which is
/// the same unknown as an unreadable ledger — it denies.
fn cmd_acquire() {
    let decision = match std::panic::catch_unwind(AssertUnwindSafe(decide)) {
        Ok(d) => d,
        Err(_) => Decision::Deny(
            "parallelguard: the concurrency gate itself crashed before it could count what is \
             in flight, so the number of concurrent calls is unknown. This call did NOT run. \
             Re-issue it on its own; if this repeats, report the crash — do not work around it \
             by batching more calls."
                .to_string(),
        ),
    };
    emit(&decision);
}

/// The PreToolUse decision. Every early return is a deny with the reason the
/// count could not be established.
fn decide() -> Decision {
    let raw = hook::read_stdin();
    let Some(input) = HookInput::parse(&raw) else {
        return Decision::Deny(undetermined_reason(
            "the hook payload on stdin was empty or did not parse as JSON",
        ));
    };
    let Some(class) = SlotClass::of_tool(&input.tool_name) else {
        // Not a metered tool. Not a verdict about it either — pass it through.
        return Decision::Allow;
    };

    let cap = harness_core::parallel::session_cap();
    let root = store::state_dir();
    let path = store::session_path(&root, &input.session_key());

    let _guard = match store::lock(&path) {
        Determination::Known(g) => g,
        Determination::Undetermined(why) => {
            return Decision::Deny(undetermined_reason(&format!(
                "the ledger lock could not be taken ({why})"
            )))
        }
    };
    let mut ledger = match store::load(&path) {
        Determination::Known(l) => l,
        Determination::Undetermined(why) => {
            return Decision::Deny(undetermined_reason(&format!(
                "the in-flight ledger could not be read ({why}). It is cleared automatically at \
                 the next turn boundary"
            )))
        }
    };

    let key = slot_key(&input);
    let now = store::now_secs();
    match ledger.acquire(class, &key, now, cap) {
        Decision::Deny(reason) => {
            log_event(
                &root,
                &format!(
                    "deny {} session={} live={} cap={}",
                    class.tag(),
                    input.session_key(),
                    ledger.count(class),
                    cap
                ),
            );
            Decision::Deny(reason)
        }
        Decision::Allow => match store::save(&path, &ledger) {
            Ok(()) => Decision::Allow,
            Err(e) => Decision::Deny(undetermined_reason(&format!(
                "the slot could not be recorded ({e}), so this call would have run outside the \
                 count and the cap would have silently risen"
            ))),
        },
    }
}

/// The shape every cannot-determine deny takes: what was unknown, that the call
/// did not run, and that this is not a judgement about the command.
fn undetermined_reason(what: &str) -> String {
    format!(
        "parallelguard could not determine how many calls are in flight: {what}. An unknown \
         count is not a free slot, so this call was refused and did NOT run (CLAUDE.md 3). This \
         is not a judgement about the command itself. Retry it; the ledger is cleared at the \
         next turn boundary."
    )
}

/// Identity of a tool call, used to pair a release with its acquire. Collisions
/// are harmless: a release that lands on a same-class sibling still decrements
/// the count by exactly one, which is all the cap depends on.
fn slot_key(input: &HookInput) -> String {
    let payload = match &input.tool_input {
        Some(v) => v.to_string(),
        None => String::new(),
    };
    harness_core::store::short_hash(&format!("{}\u{1}{payload}", input.tool_name))
}

/// Write the PreToolUse decision to stdout. `Allow` prints NOTHING — that is
/// the protocol, and it is why no failure path may exit silently.
fn emit(decision: &Decision) {
    if let Decision::Deny(reason) = decision {
        println!("{}", deny_json(reason));
    }
}

/// The one-line JSON Claude Code reads from a PreToolUse hook's stdout.
fn deny_json(reason: &str) -> String {
    serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": reason,
        }
    })
    .to_string()
}

// ---------------------------------------------------------------- release

/// PostToolUse entry point: the call finished, so give its slot back.
///
/// PostToolUse has no permission channel, so there is nothing to emit and
/// nothing to block. A failure here leaks a slot, which is the restrictive
/// direction (the cap gets tighter, never looser) and is cleared at the next
/// turn boundary — but it is invisible on stderr, so it goes to the event log
/// where `status` will show it.
fn cmd_release() {
    let raw = hook::read_stdin();
    let root = store::state_dir();
    let Some(input) = HookInput::parse(&raw) else {
        log_event(
            &root,
            "release: payload did not parse; a slot may be held until reset",
        );
        return;
    };
    let Some(class) = SlotClass::of_tool(&input.tool_name) else {
        return;
    };
    let path = store::session_path(&root, &input.session_key());
    let _guard = match store::lock(&path) {
        Determination::Known(g) => g,
        Determination::Undetermined(why) => {
            log_event(
                &root,
                &format!("release: lock unavailable ({why}); slot held until reset"),
            );
            return;
        }
    };
    let mut ledger = match store::load(&path) {
        Determination::Known(l) => l,
        Determination::Undetermined(why) => {
            log_event(
                &root,
                &format!("release: ledger unreadable ({why}); slot held until reset"),
            );
            return;
        }
    };
    if ledger.release(class, &slot_key(&input), store::now_secs()) {
        if let Err(e) = store::save(&path, &ledger) {
            log_event(
                &root,
                &format!("release: could not persist ({e}); slot held until reset"),
            );
        }
    }
}

// ------------------------------------------------------------------ reset

/// Turn-boundary entry point (SessionStart / UserPromptSubmit / Stop): drop the
/// ledger.
///
/// This is what keeps every fail-closed deny recoverable without a human. At a
/// turn boundary nothing this gate meters can still be in flight, so anything
/// left in the ledger leaked (a call the user rejected, a hook killed
/// mid-flight) and holding onto it would only shrink the width for no reason.
fn cmd_reset() {
    let raw = hook::read_stdin();
    let input = HookInput::parse(&raw).unwrap_or_default();
    let root = store::state_dir();
    store::reset(&store::session_path(&root, &input.session_key()));
}

// ----------------------------------------------------------------- status

/// Append one diagnostic line. Best-effort: the log is an observability aid,
/// never an input to a decision, so a failed write cannot change a verdict.
fn log_event(root: &std::path::Path, line: &str) {
    use std::io::Write;
    let path = root.join("events.jsonl");
    if let Ok(meta) = std::fs::metadata(&path) {
        if meta.len() > MAX_EVENT_LOG_BYTES {
            let _ = std::fs::remove_file(&path);
        }
    }
    if std::fs::create_dir_all(root).is_err() {
        return;
    }
    let record = serde_json::json!({ "at": store::now_secs(), "msg": line }).to_string();
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(f, "{record}");
    }
}

/// Operator readout for the live state dir and the cap in force.
fn render_status() -> String {
    render_status_at(&store::state_dir(), harness_core::parallel::session_cap())
}

/// Operator readout. Never prints an empty body: "nothing in flight" and "this
/// hook has never run" look identical in an empty ledger, so the two are named
/// apart explicitly.
///
/// Takes its root and cap as arguments rather than reading the environment, so
/// tests never have to mutate process-global state (which would race with every
/// other test in this binary).
fn render_status_at(root: &std::path::Path, cap: usize) -> String {
    let mut out = String::new();
    out.push_str("parallelguard — per-session concurrency cap\n");
    out.push_str(&format!(
        "cap: {cap} concurrent {} and {cap} concurrent {} per session\n",
        SlotClass::Shell.label(),
        SlotClass::Subagent.label()
    ));
    out.push_str(&format!(
        "ceiling: {} (env {} may lower it, never raise it)\n",
        harness_core::parallel::SESSION_MAX_PARALLEL,
        harness_core::parallel::ENV_OVERRIDE
    ));
    out.push_str(&format!("state: {}\n", root.display()));

    let sessions_dir = root.join("sessions");
    match harness_core::boundary::read_dir_entries(&sessions_dir) {
        Determination::Undetermined(why) => {
            out.push_str(&format!(
                "sessions: UNKNOWN — the state directory could not be listed ({why}). This is \
                 not 'nothing in flight'.\n"
            ));
        }
        Determination::Known(entries) => {
            let ledgers: Vec<_> = entries
                .iter()
                .filter(|p| p.extension().is_some_and(|e| e == "json"))
                .collect();
            if ledgers.is_empty() {
                out.push_str(
                    "sessions: none on record — either no metered call has run yet, or the \
                     PreToolUse hook is not wired up. Run a Bash call and look again; if this \
                     stays empty the gate is NOT running.\n",
                );
            } else {
                out.push_str("sessions:\n");
                for p in ledgers {
                    let name = p
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("<unnamed>");
                    match store::load(p) {
                        Determination::Known(l) => out.push_str(&format!(
                            "  {name}  shell {}/{cap}  subagent {}/{cap}  updated_at={}\n",
                            l.count(SlotClass::Shell),
                            l.count(SlotClass::Subagent),
                            l.updated_at
                        )),
                        Determination::Undetermined(why) => {
                            out.push_str(&format!("  {name}  UNREADABLE ({why})\n"));
                        }
                    }
                }
            }
        }
    }

    let events = root.join("events.jsonl");
    match harness_core::boundary::read_to_string(&events) {
        Determination::Known(Some(text)) => {
            let tail: Vec<&str> = text.lines().rev().take(10).collect();
            out.push_str("recent events (newest first):\n");
            for line in tail {
                out.push_str(&format!("  {line}\n"));
            }
        }
        Determination::Known(None) => {
            out.push_str("recent events: none recorded\n");
        }
        Determination::Undetermined(why) => {
            out.push_str(&format!("recent events: UNKNOWN ({why})\n"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use model::Inflight;
    use serde_json::json;

    fn payload(tool: &str, session: &str, input: serde_json::Value) -> String {
        json!({
            "session_id": session,
            "hook_event_name": "PreToolUse",
            "tool_name": tool,
            "tool_input": input,
        })
        .to_string()
    }

    #[test]
    fn a_deny_line_has_the_shape_claude_code_reads() {
        let s = deny_json("because");
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["hookSpecificOutput"]["hookEventName"], "PreToolUse");
        assert_eq!(v["hookSpecificOutput"]["permissionDecision"], "deny");
        assert_eq!(
            v["hookSpecificOutput"]["permissionDecisionReason"],
            "because"
        );
        assert!(!s.contains('\n'), "the decision must be one line");
    }

    #[test]
    fn the_same_call_hashes_to_the_same_slot_key() {
        // Acquire and release must agree, or every release leaks a slot.
        let a = HookInput::parse(&payload("Bash", "s", json!({"command": "ls -l"}))).unwrap();
        let b = HookInput::parse(&payload("Bash", "s", json!({"command": "ls -l"}))).unwrap();
        assert_eq!(slot_key(&a), slot_key(&b));
    }

    #[test]
    fn different_calls_hash_differently() {
        let a = HookInput::parse(&payload("Bash", "s", json!({"command": "ls"}))).unwrap();
        let b = HookInput::parse(&payload("Bash", "s", json!({"command": "pwd"}))).unwrap();
        assert_ne!(slot_key(&a), slot_key(&b));
    }

    #[test]
    fn an_allow_prints_nothing_and_a_deny_prints_one_line() {
        // The protocol: silence IS an allow. Anything that must not be read as
        // an allow therefore has to print.
        assert!(matches!(Decision::Allow, Decision::Allow));
        let d = Decision::Deny("r".into());
        match &d {
            Decision::Deny(r) => assert!(deny_json(r).contains("\"deny\"")),
            Decision::Allow => panic!("unreachable"),
        }
    }

    #[test]
    fn every_undetermined_reason_says_the_call_did_not_run() {
        let r = undetermined_reason("the ledger was on fire");
        assert!(r.contains("did NOT run"), "{r}");
        assert!(r.contains("the ledger was on fire"), "{r}");
        assert!(
            r.contains("not a judgement about the command"),
            "a cannot-determine must not read as a verdict on the command: {r}"
        );
    }

    #[test]
    fn status_names_an_empty_state_dir_as_unproven_not_as_idle() {
        let dir = tempfile::tempdir().unwrap();
        let out = render_status_at(dir.path(), 3);
        assert!(
            out.contains("the gate is NOT running"),
            "an empty store must not be reported as a healthy idle session: {out}"
        );
    }

    #[test]
    fn a_ledger_with_slots_is_reported_with_its_counts() {
        let dir = tempfile::tempdir().unwrap();
        let path = store::session_path(dir.path(), "sess-1");
        let mut l = Inflight::default();
        let _ = l.acquire(SlotClass::Shell, "a", 5, 3);
        let _ = l.acquire(SlotClass::Subagent, "b", 6, 3);
        store::save(&path, &l).unwrap();
        let out = render_status_at(dir.path(), 3);
        assert!(out.contains("sess-1"), "{out}");
        assert!(out.contains("shell 1/3"), "{out}");
        assert!(out.contains("subagent 1/3"), "{out}");
    }

    #[test]
    fn status_reports_an_unlistable_state_dir_as_unknown_not_as_empty() {
        // A state dir that cannot be listed is not an idle session.
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("sessions");
        std::fs::write(&blocker, b"not a directory").unwrap();
        let out = render_status_at(dir.path(), 3);
        assert!(
            out.contains("UNKNOWN"),
            "an unlistable store must be reported as unknown: {out}"
        );
    }
}
