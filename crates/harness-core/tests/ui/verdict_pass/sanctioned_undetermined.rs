//! Positive control for the `Undet` payload: the contract must reject *minting*
//! without also breaking the things gates legitimately do with an undetermined.
//!
//! Without this fixture the negative controls are satisfiable by a payload that
//! is simply unusable, which would push callers back toward stringly-typed
//! workarounds. The three uses below are the ones that appear across the fleet:
//! construct via the recording constructor, read the message, and forward an
//! existing one upward. (backlog 6d493e39)

use harness_core::verdict::{Determination, Verdict};

/// Forwarding: the caller matched an `Undetermined` and re-raises it unchanged.
/// This must stay writeable — and must NOT re-record, since the origin already
/// counted this give-up once.
fn forward(d: Determination<u8>) -> Determination<u8> {
    match d {
        Determination::Undetermined(why) => Determination::Undetermined(why),
        Determination::Known(v) => Determination::Known(v),
    }
}

/// Forwarding across the two enums: a `Determination` that could not be read
/// becomes an `Undetermined` verdict carrying the same payload.
fn forward_across(d: Determination<u8>) -> Verdict {
    match d {
        Determination::Undetermined(why) => Verdict::Undetermined(why),
        Determination::Known(_) => Verdict::from_findings(vec![]),
    }
}

fn main() {
    // The sanctioned minting path.
    let v = Verdict::undetermined("git rev-parse exited 128");

    // Reading the message: `as_str`, `Display`, and `Debug` all stay available,
    // so existing `{why}` / `{why:?}` / `why.as_str()` call sites keep working.
    match &v {
        Verdict::Undetermined(why) => {
            let _s: &str = why.as_str();
            let _owned: String = why.to_string();
            let _shown = format!("{why} / {why:?}");
            let _r: &harness_core::verdict::Reason = why.reason();
        }
        _ => unreachable!("just constructed an Undetermined"),
    }

    let d: Determination<u8> = Determination::undetermined("ledger unreadable");
    let _forwarded = forward(d);
    let _across = forward_across(Determination::undetermined("still unreadable"));
}
