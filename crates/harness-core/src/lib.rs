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
pub mod pricing;
pub mod projkey;
pub mod retrieval;
pub mod scorer;
pub mod session;
pub mod shell;
pub mod spans;
pub mod store;
pub mod transcript;
pub mod trust;
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
