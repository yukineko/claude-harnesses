//! Fabricating a `Clean` from `donegate`'s vantage point must be a compile
//! error, exactly as it is from any other outside-`harness-core` crate.
//!
//! `Verdict::Clean` carries an `Evidence(())` witness whose sole field is a
//! private `()`. `donegate` can *name* `Evidence` and *match* `Clean(_)`,
//! but it cannot construct `Evidence(())` — the private field forbids the
//! tuple constructor. Intended failure: private field / private constructor,
//! NOT a typo or an unknown path.

fn main() {
    let _forged = harness_core::verdict::Verdict::Clean(harness_core::verdict::Evidence(()));
}
