//! Positive control: the sanctioned paths MUST compile.
//!
//! The negative fixtures prove fabrication is rejected; this proves the contract
//! does not over-block. A `Clean` is obtainable the sanctioned way (through
//! `from_findings` / `adjudicate`, which mint the `Evidence` internally after the
//! caller expresses zero findings), and a `Known` value is extractable via the
//! one sanctioned extractor, `require`.
//!
//! `require` now yields `Required<T>` rather than `std::Result` (so that
//! `.require().unwrap_or_default()` is an E0599 — see
//! `../verdict/require_result_erasure.rs`). This fixture is the control proving
//! that seal did not also take out legitimate consumption: the two-armed `match`
//! and the panicking `expect` must both keep compiling.

use harness_core::verdict::{Determination, Required, Verdict};

fn main() {
    // Sanctioned Clean-minting path #1: an in-memory check that hands over the
    // findings it collected. Empty findings -> Clean.
    let _clean: Verdict = Verdict::from_findings(vec![]);

    // Sanctioned Clean-minting path #2: a check that may fail to run, expressing
    // a ran-and-empty outcome as `Known(vec![])`.
    let _clean2: Verdict = Verdict::adjudicate(Determination::Known(vec![]));

    // The one sanctioned extractor for a determined value, resolved the ordinary
    // way: both arms written out, the blocked one returning the fail-closed
    // verdict it was handed.
    fn judge(d: Determination<u8>) -> Verdict {
        let observed = match d.require() {
            Required::Determined(v) => v,
            Required::Blocked(verdict) => return verdict, // fail closed
        };
        if observed == 5 {
            Verdict::from_findings(vec![])
        } else {
            Verdict::violation("not five")
        }
    }
    assert!(!judge(Determination::Known(5u8)).blocks());
    assert!(judge(Determination::undetermined("could not look")).blocks());

    // The panicking shortcut stays available (it substitutes no value, so it is
    // not a permissive default).
    let six: u8 = Determination::Known(6u8)
        .require()
        .expect("Known must extract");
    assert_eq!(six, 6);
}
