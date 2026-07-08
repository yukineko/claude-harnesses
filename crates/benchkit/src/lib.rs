//! benchkit — external-benchmark (SWE-bench Verified) runner for the harness
//! monorepo.
//!
//! This is the skeleton slice: the typed [`model::Instance`], a deterministic
//! fixture-based JSONL [`loader`], and a gated [`download`] subcommand. The
//! scorer / dashboard / harness layers land in later tasks and will declare
//! their own `pub mod` here.
//!
//! Design invariant: the *loading* path is pure and deterministic — no network,
//! no clock, no env — so tests are hermetic. Only [`download`] touches the
//! network, and only when explicitly invoked.

pub mod dashboard;
pub mod download;
pub mod loader;
pub mod model;
pub mod scorer;

pub use loader::load_instances;
pub use model::Instance;
