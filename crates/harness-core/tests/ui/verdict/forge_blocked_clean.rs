//! Building a `Required::Blocked` that does not block, from outside the crate,
//! must be a compile error (backlog 1a6c1c48; formerly hole 3 of
//! `../verdict_known_holes/unsealed_paths.rs`).
//!
//! `Required::Blocked` used to carry any `Verdict`, and a `Clean` is obtainable
//! through the sanctioned `Verdict::from_findings`. So an external crate could
//! write `Required::Blocked(Verdict::from_findings(vec![]))`, and a consumer's
//! ordinary `Blocked(v) => return v` arm would then return a verdict that does
//! not block. The payload is now `Undet` — whose field is private, so it can be
//! forwarded but never minted here — and a `Verdict` is no longer accepted.
//! Intended failure: mismatched types (a `Verdict` is not an `Undet`), NOT a
//! typo or an unknown path.

use harness_core::verdict::{Required, Verdict};

fn main() {
    let _not_blocking: Required<u8> = Required::Blocked(Verdict::from_findings(vec![]));
}
