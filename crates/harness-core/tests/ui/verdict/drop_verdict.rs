//! A computed `Verdict` must not be silently dropped unread.
//!
//! `Verdict` is `#[must_use]`, so computing one and discarding it is a lint. Under
//! `#![deny(unused_must_use)]` that lint is a hard compile error — pinning that a
//! gate cannot evaluate a verdict and then fail to act on it. Intended failure:
//! `unused_must_use` denied.

#![deny(unused_must_use)]

fn main() {
    harness_core::verdict::Verdict::from_findings(vec![]);
}
