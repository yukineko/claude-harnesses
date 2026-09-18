//! context-governor — a thin control layer around Claude Code's *built-in*
//! compaction. It does NOT build a context manager from scratch; it adds four
//! things around the harness's existing compaction: **pin**, **lossless-recall**,
//! **retrieval**, and **tool-hygiene**.
//!
//! The whole design hinges on separating the three axes this layer touches —
//! conflating them was the v1/v2 design error. Every type and trait below is
//! annotated with the axis it serves:
//!
//! * **size** (window occupancy / memory): actually make the window smaller.
//!   The only levers that move size are (1) minimizing resident normative text,
//!   (2) pushing reference bodies out to retrieval, and (3) per-turn
//!   tool-result grooming. Cache placement, pinning, and lowering the
//!   auto-compact threshold do NOT reduce size.
//! * **cost** (recompute / latency): make prefill cheap = prompt cache. Stable
//!   prefixes win here; rewriting the prefix every turn loses it.
//! * **correctness** (norm preservation): stop norms / verbatim-required info
//!   from silently vanishing in a summary.
//!
//! Phase 1 (this commit) freezes the contract: the lane/spec types, the hook
//! I/O envelope, the handler trait set, and the invariants — compiling on
//! `todo!()`. Phase 2 fills the default handlers in the priority order from the
//! brief (groomer first). The bin only dispatches on `hook_event_name`.
//!
//! Reused substrate (not re-implemented here): the hook payload struct and the
//! never-break-a-turn wrapper come from [`harness_core::hook`]; the durable
//! note store, transcript streaming, and the metrics sink come from the same
//! crate. This crate adds only the governor-specific contract on top.

pub mod backing;
pub mod codesearch;
pub mod handlers;
pub mod io;
pub mod ledger;
pub mod types;

pub mod defaults;

// The contract surface the bin and the acceptance tests import. `HookInput`,
// `read_stdin`, and `run_hook` are deliberately re-exported from harness-core
// rather than redefined: the canonical payload schema (field `tool_response`,
// the empty/invalid → None parse, the panic-swallowing wrapper) is a harness
// invariant and must not drift into a private copy.
pub use harness_core::hook::{read_stdin, run_hook, HookInput};

pub use handlers::{
    Checkpointer, CompactDecision, CompactionGuard, ContextInjector, DelegateToBuiltinCompaction,
    SpecClassifier, StateRehydrator, ToolResultGroomer,
};
pub use io::{HookOutput, HookSpecific};
pub use ledger::{rollup, Action, Ledger, LedgerNode, LedgerSummary};
pub use types::{
    ContextItem, Evictable, ItemBody, ItemId, Lane, Overrun, SpecClass, StandingBudget, StoreKey,
};
