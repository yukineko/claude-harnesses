//! harness-core — the single source of truth for the unchanging infrastructure
//! shared across the yukineko Claude Code harness plugins.
//!
//! This crate is a BUILD-TIME dependency only: each plugin links it statically
//! into its self-contained binary, so the distributed `crates/<plugin>/bin/`
//! never references `../harness-core` at runtime.
//!
//! What lives here is the part that MUST be identical in every plugin —
//! especially the parallel-session-safe note store and the never-break-a-turn
//! hook wrapper (see the harness invariants). Plugin-specific domain logic and
//! config/metrics *fields* stay in each plugin crate and compose these.

// never-break-a-turn invariant backstop: the exit-0-on-error guarantee relies on
// std::panic::catch_unwind in hook::run_hook and gate::run_guarded. Under
// panic="abort" catch_unwind is a silent NO-OP and a panicking hook would abort
// the process, breaking the turn. Fail the build loudly instead of silently
// disabling the guarantee. (cfg(panic) predicate is stable since Rust 1.60.)
#[cfg(not(panic = "unwind"))]
compile_error!(
    "harness-core requires panic=\"unwind\": catch_unwind in hook::run_hook and \
     gate::run_guarded is a NO-OP under panic=\"abort\", which would break the \
     never-break-a-turn (exit-0-on-error) invariant. Restore panic=\"unwind\"."
);

pub mod append;
pub mod boundary;
pub mod code_index;
pub mod config;
pub mod daily;
pub mod degrade;
pub mod discovery;
pub mod estimate;
pub mod fleet;
pub mod gate;
pub mod git_probe;
pub mod hash;
pub mod hook;
pub mod hook_latency;
pub mod inject;
pub mod inject_metrics;
pub mod install;
pub mod interrogate;
pub mod ledger;
pub mod lessons;
pub mod metrics;
pub mod plugin_bin;
pub mod pricing;
pub mod progress;
pub mod projkey;
pub mod retrieval;
pub mod scorer;
pub mod session;
pub mod shell;
pub mod spans;
pub mod store;
pub mod transcript;
pub mod trust;
pub mod undetermined;
pub mod usage;
pub mod verdict;

// The one bundled transcript → (usage, cost) estimator, re-exported at the crate
// root so consumers call a single API instead of pairing usage::aggregate with
// pricing::session_cost by hand.
pub use estimate::{estimate_transcript_cost, TranscriptCostEstimate};
// The deterministic priority scorer (compass ONE #4), re-exported at the crate
// root so consumers call `harness_core::score(...)` directly instead of
// reaching into the `scorer` module.
pub use scorer::{score, Candidate, Effort, Lens, Severity};

/// The ONE process-wide lock for tests that mutate process-global state
/// (`HOME`, `HARNESS_TRUST_ALL`, `CONTEXT_GOVERNOR_STATE_DIR`, ...).
///
/// One lock, not one per module, because environment variables belong to the
/// *process* and every `#[test]` in this crate's `--lib` binary shares it.
/// Three modules used to hold a private `Mutex` each — `config`, `store`,
/// `undetermined` — and each carried a comment reading as though the hazard
/// were handled, while serializing only its own cases. `trust::tests` set
/// `HOME` with no lock at all.
///
/// That was not theoretical. Measured 2026-08-06 at `90385f84`:
/// `cargo test --workspace --no-fail-fast` failed 2 of 3 runs with
/// `trust.rs:274` "added project must be trusted" — `HOME` moved between
/// `add(root)` and `is_trusted(root)`, so the trust file was written to one
/// directory and read from another. Run in isolation
/// (`cargo test -p harness-core --lib`, 6 consecutive runs) it was green every
/// time: the race only opens under the CPU contention of a workspace run,
/// which is exactly the condition that makes a flake read as "unrelated".
///
/// Any new test that calls `set_var`/`remove_var` must hold this guard for its
/// whole body, including the restore. **So must any test that merely READS
/// that state more than once** — `config::tests::base_dir_is_dotprefixed_under_home`
/// resolves `HOME` on both sides of its assertion, so a concurrent `set_var`
/// between the two reads fails what is otherwise a tautology. Serializing only
/// the writers moves the flake, it does not remove it.
///
/// **Take it exactly once per test.** `std::sync::Mutex` is not reentrant, so a
/// second `lock()` on the same thread deadlocks — it does not panic, so the
/// symptom is a test binary that never finishes, not a failure with a name.
/// The rule above ("hold this guard for the whole body") therefore does NOT
/// mean "add a guard on top of a helper that already takes one":
/// `undetermined::tests::temp_env` locks internally, so its callers must not
/// hold the guard. Nothing currently enforces this beyond the present sentence
/// (backlog `3ccedd83`).
#[cfg(test)]
pub(crate) mod test_env {
    use std::sync::{Mutex, MutexGuard};

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Poisoning is deliberately recovered from rather than propagated: one
    /// panicking test would otherwise turn every later env test into a
    /// failure, burying the first (and only real) one.
    pub(crate) fn lock() -> MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }
}
