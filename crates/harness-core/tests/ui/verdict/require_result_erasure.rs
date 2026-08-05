//! `Determination::require()` (verdict.rs:407) returns `std::Result<T,
//! Verdict>`. `Determination` itself seals the permissive extractors
//! (`unwrap_or` / `ok` / `unwrap_or_default` — see determination_unwrap_or.rs),
//! but that seal is one layer too shallow: `Result`'s *own* inherent methods
//! reopen exactly the same hole one call later, and `Result::unwrap_or` has no
//! way to bound the `Err` side because it is `std`, not this crate.
//!
//! THIS IS A KNOWN-RED NEGATIVE CONTROL, not a typo. All three forms below
//! compile successfully TODAY (measured against boundary::read_dir_entries,
//! whose Undetermined arm is the fail-closed "could not list the directory"
//! answer): `unwrap_or_default()` and `unwrap_or(Vec::new())` silently collapse
//! Undetermined into an empty Vec (read as "directory has no entries", i.e.
//! clean, not "could not read the directory"), and `is_ok()` collapses the
//! whole Verdict into a bool that reads Undetermined as `false` ("not ok"),
//! indistinguishable from a legitimate absence.
//!
//! Because `compile_fail` fixtures are expected to FAIL to compile, and these
//! three currently succeed, `cargo test -p harness-core --test
//! verdict_compile_fail` is expected to report this fixture as a trybuild
//! failure ("expected test to fail to compile, but it compiled successfully").
//! That failure is the intended observation of this fixture — it is the RED
//! that proves the erasure is real and still open at the `require()` call
//! site. Do NOT add a `.stderr` snapshot or otherwise weaken this fixture to
//! make it pass; it must stay red until `require()`'s `Result<T, Verdict>` is
//! replaced or wrapped by a type that cannot be collapsed by `unwrap_or` /
//! `unwrap_or_default` / `is_ok`.

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
