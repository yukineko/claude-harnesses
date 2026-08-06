//! `Determination::require()` must not hand the caller a type whose own
//! inherent methods re-open the collapse `Determination` refuses to offer.
//!
//! HISTORY (kept, because the fixture's value is that it was red first):
//! `require()` used to return `std::Result<T, Verdict>`. `Determination` seals
//! the permissive extractors (`unwrap_or` / `ok` / `unwrap_or_default` — see
//! determination_unwrap_or.rs), but that seal was one layer too shallow —
//! `Result`'s own inherent methods reopened exactly the same hole one call
//! later, and this crate cannot remove or bound them because they are `std`'s.
//! All three forms below COMPILED SUCCESSFULLY at that point, so this fixture
//! was committed as a KNOWN-RED negative control (trybuild reported "expected
//! test case to fail to compile, but it succeeded"), and that red was the
//! observation proving the erasure was real and reachable.
//!
//! It is now GREEN because `require()` returns `harness_core::verdict::Required`,
//! which has no `unwrap_or`, `unwrap_or_default`, `unwrap_or_else`, `ok`, or
//! `is_ok` — every form below is an E0599, recorded in the `.stderr` snapshot
//! beside this file. The three forms are unchanged from the red revision; only
//! this prose was updated. Measured against `boundary::read_dir_entries`, whose
//! Undetermined arm is the fail-closed "could not list the directory" answer:
//! `unwrap_or_default()` / `unwrap_or(Vec::new())` would collapse it into an
//! empty Vec (read downstream as "directory has no entries", i.e. clean), and
//! `is_ok()` would flatten the whole Verdict into a bool where "undetermined"
//! and "legitimately absent" are the same `false`.
//!
//! Do NOT weaken this fixture (deleting a form, or relaxing the `.stderr` to a
//! non-E0599 error) to make it pass. What it does NOT claim to cover: a
//! hand-written `match ... { Required::Blocked(_) => Vec::new() }` is still
//! writeable by design — see the `Required` docs; that residue belongs to a
//! lexical gate (backlog b4baf3d7), not to this type.

use harness_core::boundary;
use std::path::{Path, PathBuf};

fn main() {
    let p = Path::new("/nonexistent-for-trybuild-require-erasure-fixture");

    // Form 1: unwrap_or_default() silently reads Undetermined as "no entries".
    let a: Vec<PathBuf> = boundary::read_dir_entries(p).require().unwrap_or_default();
    let _ = a;

    // Form 2: unwrap_or(Vec::new()) is the same collapse, spelled explicitly.
    let b: Vec<PathBuf> = boundary::read_dir_entries(p).require().unwrap_or(Vec::new());
    let _ = b;

    // Form 3: is_ok() collapses Undetermined into `false`, same bucket as a
    // legitimate "not present" answer.
    let c: bool = boundary::read_dir_entries(p).require().is_ok();
    let _ = c;
}
