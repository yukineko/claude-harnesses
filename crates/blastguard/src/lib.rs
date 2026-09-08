// テスト内の unwrap/expect は意図的な assert であって fail-open ではないので許可する。
// production 側は workspace の [workspace.lints.clippy] で deny のまま。
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
//! blastguard as a library: the pure destructive-operation detector reused by
//! other harness crates (e.g. specguard's forge validates an LLM-generated
//! `test_cmd` with [`detect::detect`] before ever handing it to `sh -c`).
//!
//! The binary (`src/main.rs`) is the PreToolUse hook; this lib exposes the same
//! detection so callers don't reimplement it. Detection is pure (no I/O): the
//! one place that needs the filesystem — resolving a destructive target's real
//! path to decide whether it lands inside this session's own tree — takes an
//! INJECTED resolver ([`scope::RealPathResolver`]) that only the binary
//! supplies. A consumer that passes none, or uses [`detect::detect`] rather
//! than [`detect::detect_scoped`], gets the strict location-blind gate.
//!
//! [`approve`] is the one module that owns state rather than a judgment: the
//! trust-on-first-use memory of which effects a human has already approved. Its
//! FINGERPRINTING is pure the same way detection is (the filesystem enters
//! through an injected [`approve::TargetProbe`]); its [`approve::Store`] does
//! read and write files, because a memory has to. Nothing in [`detect`] calls
//! it, and it can only ever downgrade an `Ask` to an `Allow` — so a library
//! consumer that never builds a `Store` gets exactly the gate it got before this
//! module existed.

pub mod approve;
pub mod callgraph;
pub mod classify;
pub mod detect;
pub mod diffrisk;
pub mod exclude;
pub mod hookio;
pub mod interactive;
pub mod model;
pub mod retro;
pub mod rule_id;
pub mod scope;
