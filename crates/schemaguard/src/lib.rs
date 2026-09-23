//! schemaguard library — schema-validation gate for LLM structured outputs.
//!
//! Exposes the same modules used by the `schemaguard` CLI so other crates
//! (e.g. `condukt`) can validate raw LLM JSON against a named schema
//! in-process, before deserializing, without shelling out to the binary.
//!
//! `clippy::panic` (along with `unwrap_used`/`expect_used`) is now enforced via
//! this crate's `[lints] workspace = true` in `Cargo.toml`, which inherits the
//! aggregated `[workspace.lints.clippy]` in the root `Cargo.toml` — this used to
//! be a hand-placed `#![deny(clippy::panic)]` here; it moved to the shared
//! aggregation point instead of being duplicated.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod metrics;
pub mod registry;
pub mod schema;
