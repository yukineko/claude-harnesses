//! A `Verdict` must never appear from `Default::default()`.
//!
//! `Verdict` deliberately implements no `Default`, so there is no "free" answer
//! (which would inevitably be a permissive Clean). Intended failure: the trait
//! bound `Verdict: Default` is not satisfied.

fn main() {
    let _free: harness_core::verdict::Verdict = Default::default();
}
