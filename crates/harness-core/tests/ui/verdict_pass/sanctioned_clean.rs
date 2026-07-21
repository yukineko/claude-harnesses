//! Positive control: the sanctioned paths MUST compile.
//!
//! The negative fixtures prove fabrication is rejected; this proves the contract
//! does not over-block. A `Clean` is obtainable the sanctioned way (through
//! `from_findings` / `adjudicate`, which mint the `Evidence` internally after the
//! caller expresses zero findings), and a `Known` value is extractable via the
//! one sanctioned extractor, `require`.

use harness_core::verdict::{Determination, Verdict};

fn main() {
    // Sanctioned Clean-minting path #1: an in-memory check that hands over the
    // findings it collected. Empty findings -> Clean.
    let _clean: Verdict = Verdict::from_findings(vec![]);

    // Sanctioned Clean-minting path #2: a check that may fail to run, expressing
    // a ran-and-empty outcome as `Known(vec![])`.
    let _clean2: Verdict = Verdict::adjudicate(Determination::Known(vec![]));

    // The one sanctioned extractor for a determined value.
    let five: u8 = Determination::Known(5u8)
        .require()
        .expect("Known must extract to Ok");
    assert_eq!(five, 5);
}
