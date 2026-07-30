//! Evidence degradation and the verdict-monotonicity property (backlog a7d41587).
//!
//! # The property
//!
//! For a gate `f`, an input `x`, and any degradation `d`:
//!
//! ```text
//!     permissiveness(f(d(x))) <= permissiveness(f(x))
//! ```
//!
//! Degrading what a gate can *see* must never move its verdict toward "fine".
//! Every fail-open this repo has fixed is an instance of that inequality being
//! violated: a failed git scan becoming an empty set, a walk error becoming a
//! partial list, a panic becoming allow, an unreadable store becoming a benign
//! default. Rather than hunting those one at a time, a crate can state the
//! property once and let proptest search for the counterexample.
//!
//! # What may be degraded, and what may NOT
//!
//! This distinction is the whole design, and getting it backwards produces a
//! property that is simply false.
//!
//! A degradation attacks the gate's **ability to observe** — the evidence is
//! truncated, corrupted, missing, unreadable. The thing being judged is
//! unchanged; only its legibility drops. Monotonicity is required here, because
//! a gate that cannot see has learned nothing, and "I could not look" is not
//! "there was nothing to find".
//!
//! It must NOT mutate the **subject** of the judgement. Truncating the command
//! `rm -rf /` to `rm -rf` legitimately yields a *more* permissive verdict: that
//! is a different, genuinely safer command, and the gate is right to say so.
//! Feeding subject-mutations to this property produces false counterexamples and
//! teaches the reader to ignore it.
//!
//! Rule of thumb: if the degradation changes what a correct, omniscient oracle
//! would answer, it is a subject-mutation and does not belong here. If the oracle
//! would give the same answer and only the gate is now blind, it belongs.
//!
//! # Deliberately no proptest dependency
//!
//! `proptest` is a dev-dependency here, and `harness-core` is linked into every
//! plugin binary. So this module carries the pure model — the degradations and
//! the ordering — and each crate writes its own strategies over
//! [`Degradation::ALL`] under its own dev-dependency.

use std::fs;
use std::io;
use std::path::Path;

/// How permissive a verdict is. Higher means closer to "carry on".
///
/// Only the boundary between *clean* and *everything else* is ranked, because
/// only that boundary is what a fail-open crosses. `Violation` and
/// `Undetermined` are both restrictive and are deliberately NOT ordered against
/// each other: turning "I found a bug" into "I could not tell" is a different
/// defect (a lost finding), it is not a fail-open, and folding both concerns
/// into one number would let this property report the wrong one.
pub trait Permissiveness {
    /// `1` for a verdict that lets work proceed, `0` for one that does not.
    fn permissiveness(&self) -> u8;
}

impl Permissiveness for bool {
    /// `true` is the permissive side. This is the right reading for the
    /// "everything is fine" flavour of boolean verdict (`converging`, `is_clean`,
    /// `allowed`). A boolean named for the PROBLEM (`has_violation`) has the
    /// opposite polarity — wrap it in `!` at the call site rather than adding a
    /// second impl, because the type cannot tell the two apart and a silent
    /// mismatch would make the property assert the reverse of what is meant.
    fn permissiveness(&self) -> u8 {
        u8::from(*self)
    }
}

impl<T: Permissiveness> Permissiveness for Option<T> {
    /// `None` — "could not reach a verdict" — is restrictive. `Some(v)` defers
    /// to `v`.
    ///
    /// The bound matters. An earlier version of this impl was `impl<T>` and
    /// returned `0` unconditionally, which ranked `Some(true)` as restrictive
    /// and made the whole property pass vacuously: nothing can be *more*
    /// permissive than a verdict that always scores zero. It was caught while
    /// writing the first consumer, not by these tests, which is why
    /// `option_some_defers_to_inner_verdict` exists below.
    fn permissiveness(&self) -> u8 {
        match self {
            None => 0,
            Some(inner) => inner.permissiveness(),
        }
    }
}

/// A degradation of a gate's evidence.
///
/// These are the shapes that have actually produced fail-opens in this repo, not
/// an abstract taxonomy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Degradation {
    /// Evidence cut off mid-record: the last entry is incomplete.
    Truncate,
    /// A byte garbled so one record no longer parses, while the rest survive.
    /// This is the sharpest of the four, because a reader that skips
    /// unparseable records silently keeps going with a *partial* history and
    /// never reports that it did.
    Corrupt,
    /// Evidence present but empty.
    Empty,
    /// Evidence present but unreadable (permissions).
    Unreadable,
}

impl Degradation {
    /// Every degradation, for exhaustive strategies:
    /// `proptest::sample::select(Degradation::ALL)`.
    pub const ALL: &'static [Degradation] = &[
        Degradation::Truncate,
        Degradation::Corrupt,
        Degradation::Empty,
        Degradation::Unreadable,
    ];

    /// Apply to a byte buffer. `Unreadable` has no in-memory meaning and yields
    /// `None` — use [`Degradation::apply_to_file`] for that one.
    ///
    /// Returns `None` when the degradation cannot change this input (e.g.
    /// truncating an empty buffer). A caller should SKIP those cases rather than
    /// count them as passing: a degradation that did not degrade proves nothing,
    /// and treating it as a pass is the same vacuity this property exists to
    /// catch.
    pub fn apply_bytes(&self, input: &[u8]) -> Option<Vec<u8>> {
        match self {
            Degradation::Empty => (!input.is_empty()).then(Vec::new),
            Degradation::Truncate => (input.len() > 1).then(|| input[..input.len() / 2].to_vec()),
            Degradation::Corrupt => {
                if input.is_empty() {
                    return None;
                }
                let mut out = input.to_vec();
                // Insert rather than overwrite: overwriting a byte can land on
                // whitespace and leave the record perfectly parseable, which
                // would make the case vacuous without saying so.
                out.insert(input.len() / 2, b'\x00');
                Some(out)
            }
            Degradation::Unreadable => None,
        }
    }

    /// Apply to a file on disk. Returns `Ok(false)` when the degradation would
    /// be a no-op for this file (same skip contract as
    /// [`Degradation::apply_bytes`]).
    pub fn apply_to_file(&self, path: &Path) -> io::Result<bool> {
        if *self == Degradation::Unreadable {
            let mut perms = fs::metadata(path)?.permissions();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                perms.set_mode(0o000);
            }
            #[cfg(not(unix))]
            {
                perms.set_readonly(true);
            }
            fs::set_permissions(path, perms)?;
            return Ok(true);
        }

        let original = fs::read(path)?;
        match self.apply_bytes(&original) {
            Some(degraded) => {
                fs::write(path, degraded)?;
                Ok(true)
            }
            None => Ok(false),
        }
    }
}

/// Does this pair satisfy monotonicity? `true` when degrading did not move the
/// verdict toward permissive.
///
/// Note the direction: equality passes. A gate whose answer is unaffected by
/// losing evidence is fine (it may not have needed that evidence); a gate whose
/// answer gets *better* is not.
pub fn is_monotone<T: Permissiveness>(original: &T, degraded: &T) -> bool {
    degraded.permissiveness() <= original.permissiveness()
}

/// Human-readable explanation of a monotonicity break, for a proptest failure
/// message. Returns `None` when the pair is monotone.
pub fn explain_break<T: Permissiveness + std::fmt::Debug>(
    degradation: Degradation,
    original: &T,
    degraded: &T,
) -> Option<String> {
    if is_monotone(original, degraded) {
        return None;
    }
    Some(format!(
        "MONOTONICITY VIOLATION under {degradation:?}: the verdict became MORE \
         permissive when evidence was degraded.\n  intact   -> {original:?}\n  \
         degraded -> {degraded:?}\nThe subject being judged did not change; only \
         the gate's ability to see it did. A gate that cannot look has not \
         learned that there is nothing to find."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_from_degraded_evidence_is_a_violation() {
        // false -> true is the shape observed in overwatch's audit ledger:
        // corrupt the file, `converging` flips to true.
        assert!(!is_monotone(&false, &true));
        assert!(explain_break(Degradation::Corrupt, &false, &true).is_some());
    }

    #[test]
    fn holding_or_tightening_is_fine() {
        // Anti-vacuity: a predicate that called everything a violation would
        // fail here.
        assert!(is_monotone(&true, &true));
        assert!(is_monotone(&false, &false));
        assert!(is_monotone(&true, &false));
        assert!(explain_break(Degradation::Corrupt, &true, &false).is_none());
    }

    #[test]
    fn option_some_defers_to_inner_verdict() {
        // The regression guard for the vacuity described on the impl. If
        // Some(true) ever ranks 0 again, every Option-valued monotonicity
        // property in the repo silently stops testing anything.
        assert_eq!(Some(true).permissiveness(), 1);
        assert_eq!(Some(false).permissiveness(), 0);
        assert_eq!(None::<bool>.permissiveness(), 0);

        // The two directions that matter for a gate reporting Option<bool>:
        // going undetermined is fine, becoming true is not.
        assert!(is_monotone(&Some(false), &None::<bool>));
        assert!(!is_monotone(&Some(false), &Some(true)));
    }

    #[test]
    fn degradations_that_cannot_bite_are_reported_as_skips() {
        // Empty input: nothing to truncate or corrupt. These must return None
        // (skip) rather than Some(unchanged), which would pass vacuously.
        assert_eq!(Degradation::Truncate.apply_bytes(b""), None);
        assert_eq!(Degradation::Corrupt.apply_bytes(b""), None);
        assert_eq!(Degradation::Empty.apply_bytes(b""), None);
        // And a single byte cannot be halved into something shorter-but-nonempty.
        assert_eq!(Degradation::Truncate.apply_bytes(b"x"), None);
    }

    #[test]
    fn each_degradation_actually_changes_the_bytes() {
        let input = b"{\"round\":\"R1\",\"new_findings\":2}\n{\"round\":\"R2\"}\n";
        for d in Degradation::ALL {
            if *d == Degradation::Unreadable {
                assert_eq!(d.apply_bytes(input), None, "Unreadable is file-only");
                continue;
            }
            let out = d.apply_bytes(input).expect("should bite on real input");
            assert_ne!(
                out.as_slice(),
                input.as_slice(),
                "{d:?} returned the input unchanged, which would pass vacuously"
            );
        }
    }

    #[test]
    fn corrupt_breaks_json_parsing_rather_than_landing_on_whitespace() {
        // The point of Corrupt is to make a record unparseable. If it landed
        // somewhere inert the case would be vacuous, so pin it.
        let line = b"{\"round\":\"R1\",\"new_findings\":2}";
        let out = Degradation::Corrupt.apply_bytes(line).unwrap();
        let text = String::from_utf8_lossy(&out);
        assert!(
            serde_json::from_str::<serde_json::Value>(&text).is_err(),
            "corrupted record still parses: {text:?}"
        );
    }
}
