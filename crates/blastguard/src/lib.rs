//! blastguard as a library: the pure destructive-operation detector reused by
//! other harness crates (e.g. specguard's forge validates an LLM-generated
//! `test_cmd` with [`detect::detect`] before ever handing it to `sh -c`).
//!
//! The binary (`src/main.rs`) is the PreToolUse hook; this lib exposes the same
//! detection so callers don't reimplement it. Detection is pure (no I/O).

pub mod classify;
pub mod detect;
pub mod diffrisk;
pub mod exclude;
pub mod hookio;
pub mod model;
