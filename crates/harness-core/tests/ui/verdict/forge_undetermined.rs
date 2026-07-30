//! Minting an `Undetermined` from outside the crate must be a compile error.
//!
//! `Verdict::undetermined` records one telemetry event per give-up, so the
//! fleet-wide rate of "could not decide" is observable. A branch that wrote the
//! variant directly would be a real give-up the counter never saw, making the
//! measurement understate itself — silently, and in exactly the direction that
//! flatters the codebase. The `Undet` payload's field is private so that bypass
//! is a compile error rather than something a reviewer has to notice.
//!
//! Intended failure: private tuple struct constructor, NOT a typo or an unknown
//! path. (backlog 6d493e39)

fn main() {
    let _forged = harness_core::verdict::Verdict::Undetermined(
        harness_core::verdict::Undet(harness_core::verdict::Reason::new("uncounted")),
    );
}
