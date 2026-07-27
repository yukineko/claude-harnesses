//! The fleet **GATE crates**: the canonical crate-name set shared by every
//! consumer that needs to know "is this change high-stakes / does it touch a
//! fleet defense gate".
//!
//! Before this module existed, the exact same six-element literal was
//! hand-copied into two separate crates — `crates/condukt/src/adversarial.rs`
//! (as `pub const GATE_CRATES: [&str; 6]`, driving the adversarial refutation
//! panel's high-stakes trigger) and `crates/tdd/src/config.rs` (as
//! `pub const GATE_CRATES: &[&str]`, driving the `strict_separation`
//! default-on gate-crate-context predicate). Both copies had, at different
//! points, silently drifted and lost `overwatch` (see the git history of
//! those two files and `scripts/check-gate-crates-sync.py`'s docstring), which
//! is exactly the failure mode a single shared source of truth removes: there
//! is now nowhere for the *value* to diverge, because there is only one value.
//!
//! `scripts/check-gate-crates-sync.py` (the machine-checked cross-source sync
//! gate for this same set across shell/Python/Markdown sources) now parses
//! this file instead of the two former Rust copies — see that script's module
//! docstring for the full source list. The Rust crates (`condukt`, `tdd`)
//! consume this constant via `pub use harness_core::fleet::GATE_CRATES;`,
//! which the Rust *compiler* keeps referentially identical to this array
//! (there is no way for a `pub use` re-export to "drift" from what it
//! re-exports — the sync script no longer has to police that direction at
//! all, only that this one file matches the other non-Rust sources).
//!
/// Must equal `scripts/rollout-plugins.sh`'s canonical `GATE_CRATES` **exactly**
/// — the same set that requires a `--canary` rollout. `scripts/continuous-audit.sh`'s
/// `DEFAULT_TARGETS` is a strict *superset* of this (it additionally carries
/// audit-only crates such as `backlog`, which are reviewed but gate nothing), so
/// it is deliberately NOT mirrored here.
///
/// `overwatch` is a member for the same reason rollout-plugins.sh includes it:
/// it is not itself a prompt-injection/spec/mutation defense gate, but it
/// computes the canary health-gate decision and records confirmed audit
/// findings. A regression in it silently removes the safety net for every
/// OTHER gate crate.
///
/// Enforced by `scripts/check-gate-crates-sync.py` (this file is the tracked
/// canonical Rust source; see that script's `SOURCES` list).
pub const GATE_CRATES: &[&str] = &[
    "blastguard",
    "propguard",
    "specguard",
    "stuckguard",
    "taintguard",
    "mutategate",
    "overwatch",
];

#[cfg(test)]
mod tests {
    use super::GATE_CRATES;

    #[test]
    fn gate_crates_is_the_known_seven() {
        assert_eq!(
            GATE_CRATES,
            &[
                "blastguard",
                "propguard",
                "specguard",
                "stuckguard",
                "taintguard",
                "mutategate",
                "overwatch"
            ]
        );
    }
}
