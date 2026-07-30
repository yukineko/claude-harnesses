//! The same bypass on the `Determination` side must also be a compile error.
//!
//! Both enums carry the same `Undet` payload precisely so the hole cannot be
//! closed on one and left open on the other — the recurring shape in this repo
//! is a fix that lands on one mirror while its twin stays open. (backlog
//! 6d493e39)

fn main() {
    let _forged: harness_core::verdict::Determination<u8> =
        harness_core::verdict::Determination::Undetermined(harness_core::verdict::Undet(
            harness_core::verdict::Reason::new("uncounted"),
        ));
}
