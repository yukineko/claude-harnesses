//! Hook dispatch binary. Reads the stdin payload, branches on
//! `hook_event_name`, runs the matching handler, and writes the envelope —
//! nothing else lives here (the contract is in the lib).
//!
//! Two execution rules from the brief are encoded in `main`:
//! * **never break a turn** — the whole dispatch runs inside
//!   `harness_core::hook::run_hook`, which swallows panics and exits 0.
//! * **PreCompact may block** — the one event allowed to exit non-zero. A
//!   `Block` decision exits 2 (Claude Code's block signal); every other path,
//!   including a `Proceed`, writes its envelope and falls through to exit 0.
//!   Stop/SubagentStop deliberately discard their output: checkpointing is a
//!   side effect and must never block.

use std::io::Write;

use context_governor::backing::TranscriptBackingStore;
use context_governor::defaults::{checkpointer, groomer, guard, injector, rehydrator};
use context_governor::handlers::{
    Checkpointer, CompactDecision, CompactionGuard, ContextInjector, StateRehydrator,
};
use context_governor::io::HookOutput;
use context_governor::LedgerSummary;
use harness_core::hook::{run_hook, try_read_stdin, HookInput};

/// What the dispatch decided to do, kept separate from doing it so `main` owns
/// the process-exit policy (the handlers never call `exit`).
enum Dispatch {
    /// Write this envelope to stdout, then exit 0.
    Emit(HookOutput),
    /// PreCompact asked to block: exit 2 with `reason` on stderr.
    Block(String),
}

/// Format a `LedgerSummary` into a human-readable, metrics-style string.
fn render_rollup(s: &LedgerSummary) -> String {
    let mut out = String::new();
    out.push_str("context-governor ledger rollup\n");
    out.push_str(&format!("  rows: {}\n", s.rows));
    out.push_str(&format!("  total_saved_tokens: {}\n", s.total_saved_tokens));
    out.push_str("  per_event:\n");
    for (event, count) in &s.per_event {
        out.push_str(&format!("    {event}: {count}\n"));
    }
    out
}

fn main() -> ! {
    if std::env::args().nth(1).as_deref() == Some("rollup") {
        let cwd = std::env::current_dir()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| ".".to_string());
        let summary = context_governor::rollup(&cwd);
        print!("{}", render_rollup(&summary));
        std::process::exit(0);
    }
    run_hook(|| {
        // A stdin READ ERROR is named on stderr, not left to the generic
        // panic path (backlog 8004d80f). The event is unknown, so nothing is
        // emitted and nothing is blocked (exit 0).
        let raw = match try_read_stdin() {
            Ok(raw) => raw,
            Err(e) => {
                eprintln!("context-governor: stdin read failed: {e}; hook event not handled");
                return;
            }
        };
        let Some(input) = HookInput::parse(&raw) else {
            return; // empty/invalid payload → silent no-op (exit 0)
        };

        match dispatch(&input) {
            Dispatch::Emit(out) => {
                // Cross-harness injection-budget observability (ADR 0001 Phase 2):
                // record the post-cap injected `additionalContext` size for the
                // UserPromptSubmit turn. Other events carry no additionalContext.
                if input.hook_event_name == "UserPromptSubmit" {
                    if let Some(ctx) = out
                        .specific
                        .as_ref()
                        .and_then(|s| s.additional_context.as_deref())
                    {
                        harness_core::inject_metrics::record(
                            "context-governor",
                            &input.session_id,
                            &input.prompt,
                            ctx.chars().count(),
                        );
                    }
                }
                // `{}` (the default) is a valid "proceed, no-op" response.
                let _ = writeln!(std::io::stdout(), "{}", out.to_json());
            }
            Dispatch::Block(reason) => {
                let _ = writeln!(std::io::stderr(), "{reason}");
                std::process::exit(2);
            }
        }
    })
}

/// Pure routing: pick the handler for the event and return what to do. Kept free
/// of process-exit / stdout so it stays unit-testable.
fn dispatch(input: &HookInput) -> Dispatch {
    // The backing store is opened lazily; the events that don't touch it still
    // compile through the same seam.
    match input.hook_event_name.as_str() {
        // ★ primary size lever
        "PostToolUse" => Dispatch::Emit(groomer().to_output(input)),

        "UserPromptSubmit" => Dispatch::Emit(injector().inject(input)),

        "SessionStart" => {
            // No store → nothing to rehydrate from; proceed silently.
            let Some(store) = open_store(input) else {
                return Dispatch::Emit(HookOutput::default());
            };
            Dispatch::Emit(rehydrator().rehydrate(input, &store))
        }

        "PreCompact" => {
            // No store → never block compaction on a store-open failure.
            let Some(mut store) = open_store(input) else {
                return Dispatch::Emit(HookOutput::default());
            };
            match guard().on_pre_compact(input, &mut store) {
                CompactDecision::Proceed => Dispatch::Emit(HookOutput::default()),
                CompactDecision::Block { reason } => Dispatch::Block(reason),
            }
        }

        // Side effects only; output discarded so checkpointing can never block.
        "Stop" | "SubagentStop" => {
            // No store → skip the checkpoint; a Stop hook must never break a turn.
            if let Some(mut store) = open_store(input) {
                checkpointer().checkpoint(input, &mut store);
            }
            Dispatch::Emit(HookOutput::default())
        }

        _ => Dispatch::Emit(HookOutput::default()),
    }
}

/// Open the backing store for this invocation. Returns `None` (fail-soft) when
/// the store cannot be opened, so a hook never panics and breaks the turn — the
/// caller falls back to a silent no-op. The contract is that losing the
/// governor's side effects is always preferable to breaking the surrounding
/// turn.
fn open_store(input: &HookInput) -> Option<TranscriptBackingStore> {
    TranscriptBackingStore::open(&input.cwd).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An unknown/unhandled event must route to a silent no-op (`{}`) without
    /// opening the store or invoking a handler — the regression guard for the
    /// `_` arm. (The live event arms reach `todo!()` until Phase 2, so they are
    /// covered once the handlers are real.)
    #[test]
    fn unknown_event_emits_empty_proceed() {
        let input = HookInput {
            hook_event_name: "Notification".to_string(),
            ..Default::default()
        };
        match dispatch(&input) {
            Dispatch::Emit(out) => assert_eq!(out.to_json(), "{}"),
            Dispatch::Block(_) => panic!("unknown event must never block"),
        }
    }

    /// Regression for the never-break-a-turn contract: `open_store` is now
    /// fail-soft (`Option`), so a store-touching event routes to a silent
    /// `Dispatch::Emit` and never panics or blocks. The `is_some()` assertion
    /// also fails to compile if a future change reverts `open_store` to a
    /// panicking `.expect()` that returns the store directly.
    #[test]
    fn store_event_is_fail_soft_and_never_breaks_a_turn() {
        let td = tempfile::tempdir().expect("tempdir");
        // Isolate state under a tempdir so the test never pollutes $HOME and
        // never reads another run's ledger.
        std::env::set_var("CONTEXT_GOVERNOR_STATE_DIR", td.path());

        let input = HookInput {
            hook_event_name: "Stop".to_string(),
            cwd: td.path().to_string_lossy().into_owned(),
            ..Default::default()
        };

        assert!(open_store(&input).is_some(), "a valid cwd opens the store");

        // A Stop hook must emit `{}` and never block the turn.
        match dispatch(&input) {
            Dispatch::Emit(out) => assert_eq!(out.to_json(), "{}"),
            Dispatch::Block(_) => panic!("Stop must never block a turn"),
        }

        std::env::remove_var("CONTEXT_GOVERNOR_STATE_DIR");
    }

    /// Pure formatting test for `render_rollup` — does not spawn a process or
    /// read the filesystem. Verifies the key fields appear in the output.
    #[test]
    fn render_rollup_formats_summary() {
        use context_governor::LedgerSummary;
        use std::collections::BTreeMap;

        let mut per_event = BTreeMap::new();
        per_event.insert("groomed".to_string(), 2u64);
        per_event.insert("injected".to_string(), 1u64);

        let summary = LedgerSummary {
            total_saved_tokens: 150,
            rows: 3,
            per_event,
        };

        let rendered = render_rollup(&summary);
        assert!(
            rendered.contains("total_saved_tokens: 150"),
            "missing total_saved_tokens in: {rendered}"
        );
        assert!(rendered.contains("rows: 3"), "missing rows in: {rendered}");
        assert!(
            rendered.contains("groomed: 2"),
            "missing groomed count in: {rendered}"
        );
        assert!(
            rendered.contains("injected: 1"),
            "missing injected count in: {rendered}"
        );
    }
}
