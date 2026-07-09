//! Re-export shim, not the real impl. The Claude Code hook stdin payload
//! (transcript path + turn metadata) lives in `harness_core::hook`; this module
//! only re-exports the canonical [`harness_core::hook::HookInput`] (covers
//! `stop_hook_active`, `transcript_path`, `project_name()`, etc.) so every
//! plugin shares one struct + parse contract. Edit the type in harness-core.

pub use harness_core::hook::HookInput;
