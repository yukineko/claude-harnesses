//! The `Deny > Ask > Allow` accumulator every re-analysis loop in `detect`
//! uses, and the boundary conversions between this crate's `Decision` and
//! `harness_core`'s `Verdict`. Moved out of `detect.rs` unchanged.

use harness_core::verdict::Verdict;

use crate::model::Decision;

/// Combines sub-analysis verdicts under the `Deny > Ask > Allow` ranking.
///
/// Every re-analysis loop in `detect` (segments, `-c` payload candidates,
/// command-word candidates, `-exec` tails) evaluates several sub-commands and
/// must report the STRONGEST verdict any of them produced. Two mistakes are
/// possible here and both are fail-opens:
///
///   * testing `is_deny()` and returning early on it alone — an `Ask` from a
///     sub-analysis is then dropped and the loop falls through to Allow;
///   * returning the FIRST blocking verdict — an early `Ask` would then mask a
///     later `Deny` on the same line, downgrading a known-destructive command
///     to a question.
///
/// So: a `Deny` short-circuits (nothing can outrank it), an `Ask` is remembered
/// but the scan continues in case a `Deny` follows, and `finish` reports the
/// remembered `Ask` only if no `Deny` was ever seen.
///
/// Internally this ranking is exactly [`harness_core::verdict::Verdict`]'s own
/// `Violation > Undetermined > Clean` priority (`Verdict::worst_of`), renamed
/// to this crate's domain vocabulary at the two edges (see
/// [`decision_to_verdict`]/[`verdict_to_decision`]) rather than re-implemented.
#[derive(Default)]
pub(super) struct VerdictAcc {
    // Holds at most one Undetermined (the first one seen) — a Violation is
    // resolved and returned by `record` immediately so it never sits here,
    // and Clean carries nothing worth keeping.
    rest: Vec<Verdict>,
}

impl VerdictAcc {
    /// Record a sub-verdict. Returns `Some(deny)` when the caller must stop and
    /// return it immediately; `None` when the scan should continue.
    pub(super) fn record(&mut self, d: Decision) -> Option<Decision> {
        match decision_to_verdict(d) {
            // Nothing recorded so far can outrank a Violation, so there is no
            // need to fold it into `rest` first — report it straight away.
            v @ Verdict::Violation(_) => Some(verdict_to_decision(v)),
            v @ Verdict::Undetermined(_) => {
                // Keep only the FIRST undetermined, so the reported reason is
                // the outermost / earliest unanalysable construct rather than
                // the last one. `Verdict::worst_of` joins every `Undetermined`
                // reason it is handed, so recording just the first is what
                // preserves that "first wins" behavior through to `finish`.
                if !self
                    .rest
                    .iter()
                    .any(|r| matches!(r, Verdict::Undetermined(_)))
                {
                    self.rest.push(v);
                }
                None
            }
            Verdict::Clean(_) => None,
        }
    }

    /// The strongest verdict seen, given no `Deny` short-circuited the caller.
    pub(super) fn finish(self) -> Decision {
        verdict_to_decision(Verdict::worst_of(self.rest))
    }
}

/// Boundary conversion: this crate's domain-specific three answers map
/// one-to-one onto `harness_core`'s generic three answers, in the same
/// priority order (`Deny`/`Violation` > `Ask`/`Undetermined` > `Allow`/`Clean`).
fn decision_to_verdict(d: Decision) -> Verdict {
    match d {
        Decision::Allow => Verdict::from_findings(vec![]),
        Decision::Deny(reason) => Verdict::violation(reason),
        Decision::Ask(reason) => Verdict::undetermined(reason),
    }
}

/// The inverse of [`decision_to_verdict`], used at the boundary where a
/// computation finishes with a generic `Verdict` and must report back in this
/// crate's own `Decision` vocabulary.
fn verdict_to_decision(v: Verdict) -> Decision {
    match v {
        Verdict::Clean(_) => Decision::Allow,
        Verdict::Violation(reason) => Decision::deny(reason.as_str().to_string()),
        Verdict::Undetermined(reason) => Decision::ask(reason.as_str().to_string()),
    }
}
