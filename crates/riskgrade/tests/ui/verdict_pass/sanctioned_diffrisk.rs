//! Positive control: the sanctioned `harness_core::verdict` path riskgrade's
//! diffrisk actually uses MUST compile and run. Moved verbatim from
//! crates/blastguard/tests/ui/verdict_pass/sanctioned_clean.rs (its third path)
//! when diffrisk moved to riskgrade on 2026-11-17; only the crate path changed.
//! `riskgrade::diffrisk::SensitiveConfig::any_sensitive` returns a
//! `Determination<bool>`, and `require()` resolves it into a plain `bool` the
//! way the migrated call sites do -- proof that the migration onto
//! `Determination` is wired to the type contract, not just declared in prose.

fn main() {
    // diffrisk's migrated call site: `any_sensitive` returns a
    // `Determination<bool>`; `require()` is the one sanctioned way to read it,
    // and a real (compiled, tested) glob config is `Known`, never
    // `Undetermined`.
    let cfg = riskgrade::diffrisk::SensitiveConfig::default();
    let sensitive: bool = cfg
        .any_sensitive(&["src/auth/login.rs".to_string()])
        .require()
        .expect("a default, always-compilable glob set is Known, not Undetermined");
    assert!(sensitive, "src/auth/** is a built-in sensitive glob");
}
