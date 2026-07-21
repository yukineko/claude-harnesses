//! Positive control: the sanctioned `harness_core::verdict` paths blastguard
//! actually uses MUST compile.
//!
//! The negative fixtures prove fabrication is rejected; this proves the
//! contract does not over-block. This mirrors
//! `harness-core/tests/ui/verdict_pass/sanctioned_clean.rs`, plus a third
//! path: `blastguard::diffrisk::SensitiveConfig::any_sensitive` returns a
//! `Determination<bool>`, and `require()` resolves it into a plain `bool` the
//! way the migrated call sites do -- proof that blastguard's own migration
//! onto `Determination` is wired to the type contract, not just declared in
//! prose.

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
        .expect("Known must extract to Ok");
    assert_eq!(five, 5);

    // blastguard's own migrated call site: `any_sensitive` returns a
    // `Determination<bool>`; `require()` is the one sanctioned way to read it,
    // and a real (compiled, tested) glob config is `Known`, never
    // `Undetermined`.
    let cfg = blastguard::diffrisk::SensitiveConfig::default();
    let sensitive: bool = cfg
        .any_sensitive(&["src/auth/login.rs".to_string()])
        .require()
        .expect("a default, always-compilable glob set is Known, not Undetermined");
    assert!(sensitive, "src/auth/** is a built-in sensitive glob");
}
