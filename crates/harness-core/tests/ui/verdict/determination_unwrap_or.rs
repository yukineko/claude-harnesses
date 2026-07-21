//! `Determination` must expose no permissive extractor.
//!
//! The whole point of `Determination` is that "could not determine" cannot be
//! swapped for a default. Its only extractor is `require()` (→ `Result`). The
//! permissive escape hatches — `unwrap_or`, `ok`, `unwrap_or_default` — must not
//! exist. Intended failure: no such method on `Determination<T>` (E0599), NOT a
//! trait-bound or typo error. Each call uses a fresh binding so the errors are
//! "method not found", never "use of moved value".

use harness_core::verdict::Determination;

fn main() {
    let a: Determination<Vec<u8>> = Determination::undetermined("x");
    let _ = a.unwrap_or(vec![]);

    let b: Determination<Vec<u8>> = Determination::undetermined("x");
    let _ = b.ok();

    let c: Determination<Vec<u8>> = Determination::undetermined("x");
    let _ = c.unwrap_or_default();
}
