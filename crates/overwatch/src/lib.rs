//! Overwatch: project-global lease ledger and cross-session dedup.
//!
//! Re-exports the public API for use by tests and other crates.
#![deny(clippy::panic)]

pub mod audit_round;
pub mod canary;
pub mod disposition;
pub mod event;
// `lock` is bin-only in `main.rs`, but `store` (in this lib) now reuses
// `LeaseLock` to serialize its shared-JSONL read-modify-write paths, so the lib
// crate root must also declare the module. Kept private: it is an internal
// concurrency primitive, not public API.
mod lock;
pub mod review_finding;
pub mod rollback;
pub mod store;
pub mod violation;
