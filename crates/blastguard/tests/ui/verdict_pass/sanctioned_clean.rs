//! Positive control: the sanctioned `harness_core::verdict` paths blastguard
//! actually uses MUST compile.
//!
//! The negative fixtures prove fabrication is rejected; this proves the
//! contract does not over-block. This mirrors
//! `harness-core/tests/ui/verdict_pass/sanctioned_clean.rs`. The third path
//! this control used to carry (`blastguard::diffrisk::SensitiveConfig::
//! any_sensitive` resolved with `require()`) moved with diffrisk to riskgrade
//! on 2026-11-17: crates/riskgrade/tests/ui/verdict_pass/sanctioned_diffrisk.rs.

use harness_core::verdict::{Determination, Verdict};

fn main() {
    // Sanctioned Clean-minting path #1: an in-memory check that hands over
    // the findings it collected. Empty findings -> Clean.
    let _clean: Verdict = Verdict::from_findings(vec![]);

    // Sanctioned Clean-minting path #2: a check that may fail to run,
    // expressing a ran-and-empty outcome as `Known(vec![])`.
    let _clean2: Verdict = Verdict::adjudicate(Determination::Known(vec![]));

    // The one sanctioned extractor for a determined value.
    let five: u8 = Determination::Known(5u8)
        .require()
        .expect("Known must extract to Required::Determined");
    assert_eq!(five, 5);
}
