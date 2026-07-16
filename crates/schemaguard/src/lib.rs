//! schemaguard library — schema-validation gate for LLM structured outputs.
//!
//! Exposes the same modules used by the `schemaguard` CLI so other crates
//! (e.g. `condukt`) can validate raw LLM JSON against a named schema
//! in-process, before deserializing, without shelling out to the binary.
#![deny(clippy::panic)]

pub mod metrics;
pub mod registry;
pub mod schema;
