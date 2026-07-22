// テスト内の unwrap/expect は意図的な assert であって fail-open ではないので許可する。
// production 側は workspace の [workspace.lints.clippy] で deny のまま。
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
//! blastguard as a library: the pure destructive-operation detector reused by
//! other harness crates (e.g. specguard's forge validates an LLM-generated
//! `test_cmd` with [`detect::detect`] before ever handing it to `sh -c`).
//!
//! The binary (`src/main.rs`) is the PreToolUse hook; this lib exposes the same
//! detection so callers don't reimplement it. Detection is pure (no I/O).
#![deny(clippy::panic)]

pub mod callgraph;
pub mod classify;
pub mod detect;
pub mod diffrisk;
pub mod exclude;
pub mod hookio;
pub mod interactive;
pub mod model;
pub mod rule_id;
