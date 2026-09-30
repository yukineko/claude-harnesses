//! The paths `harness_core::verdict` does NOT seal, pinned as compiling and
//! running (backlog 5b89f0f6).
//!
//! verdict.rs used to say the permissive path "is not expressible as a method
//! call on either type" and that `Determination` "has exactly one extractor".
//! An adversarial audit (2026-08-06) built an external crate against the real
//! harness-core and refuted both. This fixture is that refutation, kept so the
//! prose cannot drift back: if one of these three holes is ever closed, this
//! fixture stops compiling (or its asserts fail), and the prose in verdict.rs
//! that names the hole must be updated in the same change.
//!
//! None of the three is reachable by accident; each is a deliberate spelling
//! that shows up in a diff. The first two are what scripts/check-fail-open.py's
//! `undetermined-arm-empty-fallback` pattern exists to see; the extension-trait
//! form is not caught by any gate today.

use harness_core::verdict::{Determination, Required, Verdict};

// Hole 1: an external extension trait gives `Required` exactly the methods the
// type withholds. Call sites then spell `.require().unwrap_or_default()` —
// byte-identical to the E0599 fixture in ../verdict/require_result_erasure.rs —
// and it compiles, because the trait is in scope here.
trait Erase<T> {
    fn unwrap_or_default(self) -> T;
    fn is_ok(&self) -> bool;
}
impl<T: Default> Erase<T> for Required<T> {
    fn unwrap_or_default(self) -> T {
        match self {
            Required::Determined(v) => v,
            Required::Blocked(_) => T::default(),
        }
    }
    fn is_ok(&self) -> bool {
        matches!(self, Required::Determined(_))
    }
}

fn main() {
    let undetermined = || Determination::<Vec<u8>>::undetermined("fixture: could not observe");

    let collapsed: Vec<u8> = undetermined().require().unwrap_or_default();
    assert!(collapsed.is_empty(), "hole 1: Undetermined collapsed to an empty Vec");
    assert!(!undetermined().require().is_ok());

    // Hole 2: `Known` is a pub variant, so `require` is not the only extractor —
    // a direct match is a second one, and it can default the other arm too.
    let direct: Vec<u8> = match undetermined() {
        Determination::Known(v) => v,
        Determination::Undetermined(_) => Vec::new(),
    };
    assert!(direct.is_empty(), "hole 2: direct match collapsed Undetermined");

    // Hole 3: `Required::Blocked` carries any `Verdict`, and a `Clean` is
    // obtainable through the sanctioned `from_findings`. So a "blocked" value
    // that does not block can be built outside harness-core.
    let not_blocking: Required<u8> = Required::Blocked(Verdict::from_findings(vec![]));
    match not_blocking {
        Required::Blocked(v) => assert!(!v.blocks(), "hole 3: Blocked(Clean) does not block"),
        Required::Determined(_) => unreachable!(),
    }
}
