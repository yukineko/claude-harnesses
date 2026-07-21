//! Fabricating a `Clean` from outside the crate must be a compile error.
//!
//! `Verdict::Clean` carries an `Evidence(())` witness whose sole field is a
//! private `()`. An external crate can *name* `Evidence` and *match* `Clean(_)`,
//! but it cannot construct `Evidence(())` — the private field forbids the tuple
//! constructor. Intended failure: private field / private constructor, NOT a
//! typo or an unknown path.

fn main() {
    let _forged = harness_core::verdict::Verdict::Clean(harness_core::verdict::Evidence(()));
}
