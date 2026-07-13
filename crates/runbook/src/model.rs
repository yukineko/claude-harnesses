//! serde struct for the Claude Code **UserPromptSubmit** hook stdin payload.
//!
//! runbook only consumes a subset of the payload (session_id, cwd,
//! hook_event_name, prompt); the shared `harness_core::hook::HookInput` is a
//! superset with every field `#[serde(default)]`, so parsing and
//! `cwd_or_current` behave identically. Re-exported to keep call sites unchanged.

pub use harness_core::hook::HookInput;
