//! A `Verdict` must not be convertible to/from `bool`.
//!
//! `From<bool>` would let `true`/`false` mint a verdict (a bool cannot represent
//! the third answer, so such a conversion is a fail-open by construction), and
//! `Into<bool>` would flatten `Undetermined` onto one side of a two-valued
//! answer. Neither impl exists. Intended failure: `From<bool>`/`Into<bool>` not
//! implemented for `Verdict`.

fn main() {
    let _from = harness_core::verdict::Verdict::from(true);

    let v = harness_core::verdict::Verdict::from_findings(vec![]);
    let _into: bool = v.into();
}
