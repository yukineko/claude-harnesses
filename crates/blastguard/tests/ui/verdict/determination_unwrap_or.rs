//! `Determination` must expose no permissive extractor, from `blastguard`'s
//! vantage point as well (it consumes `Determination<bool>` via
//! `SensitiveConfig::any_sensitive`).
//!
//! Its only extractor is `require()` (-> `Required<T>`). The permissive escape
//! hatches -- `unwrap_or`, `ok`, `unwrap_or_default` -- must not exist.
//! Intended failure: no such method on `Determination<T>` (E0599), NOT a
//! trait-bound or typo error. Each call uses a fresh binding so the errors
//! are "method not found", never "use of moved value".

use harness_core::verdict::Determination;

fn main() {
    let a: Determination<bool> = Determination::undetermined("x");
    let _ = a.unwrap_or(false);

    let b: Determination<bool> = Determination::undetermined("x");
    let _ = b.ok();

    let c: Determination<bool> = Determination::undetermined("x");
    let _ = c.unwrap_or_default();
}
