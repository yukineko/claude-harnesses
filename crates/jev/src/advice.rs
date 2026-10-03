//! [`Advice`] — the only shape in which a jev answer is allowed to leave this
//! crate, and the reason a missing API key is safe by construction.
//!
//! # Two answers, and neither of them is "fine"
//!
//! The repo's gate types are three-valued because the recurring defect is
//! "could not check" collapsing into "clean"
//! (`harness_core::verdict`, `crates/blastguard/src/model.rs:5`). An *advisor*
//! has the mirror-image hazard, and it needs a different fix.
//!
//! jev is a closed third-party service, so CLAUDE.md §7 forbids it from
//! holding the authority to pass or stop anything. The failure mode to design
//! against is therefore not "undetermined read as clean" — it is **"jev was
//! not consulted, read as jev approving"**. A three-valued
//! `Escalate / Clean / Undetermined` would reintroduce exactly that: a
//! consumer would write `match advice { Clean => proceed, Undetermined =>
//! proceed, .. }`, and the day the key expired, every call site would start
//! silently taking the `proceed` arm for a different reason than it thought.
//!
//! So `Advice` is two-valued in the *other* direction:
//!
//! - [`Advice::Escalate`] — jev answered, and the answer is a reason to treat
//!   the subject **more** restrictively (rank it higher, warn a human, attach
//!   a note).
//! - [`Advice::NoSignal`] — everything else, with the reason kept for
//!   telemetry but **not** for control flow: not configured, environment
//!   unreadable, HTTP error, timeout, unparseable body, a probability below
//!   the caller's threshold, and jev actively answering "no, this is fine".
//!
//! The last item in that list is the load-bearing one. "jev says it is fine"
//! is *deliberately indistinguishable* from "jev never ran". A consumer cannot
//! branch on the difference because the difference is not representable, so:
//!
//! - deleting `TYPESAFE_API_KEY` cannot change any consumer's behaviour except
//!   by removing escalations, and
//! - no consumer can be written that *relies* on jev to clear something,
//!   because there is no value meaning "cleared".
//!
//! That is what "advisory" means here, enforced rather than promised. There is
//! no `Default` (a default `Advice` would be a free answer), no `From<bool>`
//! (a bool cannot carry a reason), and no conversion to
//! `harness_core::verdict::Verdict` in either direction.
//!
//! **Do not add a `Clean` variant.** If a task seems to need one, the task is
//! asking jev to hold gate authority, which is the §7 violation this file
//! exists to make unrepresentable.

use crate::wire::Answer;

/// Why there is nothing to say. Kept for the ledger and for `--explain`
/// output; **never** for control flow.
#[derive(Debug, Clone, PartialEq)]
pub enum NoSignalReason {
    /// No key, or the environment could not be read. Carries the
    /// human-readable availability reason.
    Unavailable(String),
    /// The call was attempted and did not produce a usable answer (HTTP
    /// status, timeout, transport failure, unparseable body, missing question
    /// key).
    CallFailed(String),
    /// jev answered, and the answer does not clear the caller's threshold.
    /// **This is not an endorsement** — see the module docs.
    BelowThreshold {
        /// The probability jev returned.
        probability: f64,
        /// The threshold the caller required.
        threshold: f64,
    },
    /// jev answered with a shape the caller did not ask for (e.g. a Score
    /// answer where a Noul was expected).
    WrongAnswerShape(String),
}

impl NoSignalReason {
    pub fn describe(&self) -> String {
        match self {
            NoSignalReason::Unavailable(why) => format!("jev unavailable: {why}"),
            NoSignalReason::CallFailed(why) => format!("jev call did not answer: {why}"),
            NoSignalReason::BelowThreshold {
                probability,
                threshold,
            } => format!("jev answered {probability:.3}, below the required {threshold:.3}"),
            NoSignalReason::WrongAnswerShape(why) => format!("jev answered the wrong shape: {why}"),
        }
    }
}

/// An advisory hint. See the module docs for why there is no third variant.
#[must_use = "an Advice is a hint that must be surfaced or discarded explicitly"]
#[derive(Debug, Clone, PartialEq)]
pub enum Advice {
    /// jev answered, and the answer argues for more restriction.
    Escalate {
        /// The question key the caller used.
        question: String,
        /// The calibrated probability (Noul) or confidence-weighted signal
        /// behind the escalation, in `0.0..=1.0`.
        probability: f64,
        /// Human-readable detail to show a person. Advisory text only.
        detail: String,
    },
    /// Nothing usable. Indistinguishable from "jev approves", on purpose.
    NoSignal { reason: NoSignalReason },
}

impl Advice {
    /// jev could not be consulted.
    pub fn unavailable(why: impl Into<String>) -> Self {
        Advice::NoSignal {
            reason: NoSignalReason::Unavailable(why.into()),
        }
    }

    /// The call happened but produced no usable answer.
    pub fn call_failed(why: impl Into<String>) -> Self {
        Advice::NoSignal {
            reason: NoSignalReason::CallFailed(why.into()),
        }
    }

    /// Interpret a Noul answer as a one-directional hint.
    ///
    /// `question` is the caller's key, `threshold` the probability at or above
    /// which the caller wants to be warned. Below it — or for any non-Noul
    /// answer — the result is [`Advice::NoSignal`]. There is no inverse
    /// constructor: a low probability cannot be turned into approval.
    pub fn from_noul(question: &str, answer: &Answer, threshold: f64) -> Self {
        let Answer::Noul { noul } = answer else {
            return Advice::NoSignal {
                reason: NoSignalReason::WrongAnswerShape(format!(
                    "question {question:?} expected a noul answer, got {}",
                    answer.kind()
                )),
            };
        };
        let p = *noul;
        if !p.is_finite() || !(0.0..=1.0).contains(&p) {
            return Advice::NoSignal {
                reason: NoSignalReason::CallFailed(format!(
                    "question {question:?} returned an out-of-range noul {p}"
                )),
            };
        }
        if p >= threshold {
            Advice::Escalate {
                question: question.to_string(),
                probability: p,
                detail: format!(
                    "jev put the probability at {p:.3} (threshold {threshold:.3}). Advisory only: \
                     this is a hint for a human or a ranking, never a gate decision."
                ),
            }
        } else {
            Advice::NoSignal {
                reason: NoSignalReason::BelowThreshold {
                    probability: p,
                    threshold,
                },
            }
        }
    }

    /// True only for [`Advice::Escalate`].
    ///
    /// Note the asymmetry, which is the contract: there is no `is_clean`. A
    /// caller can ask "should I raise this?" and can never ask "did jev clear
    /// this?".
    pub fn is_escalation(&self) -> bool {
        matches!(self, Advice::Escalate { .. })
    }

    /// One line for a human. Empty for `NoSignal`, so rendering an absent
    /// advisor adds nothing to the output.
    pub fn render(&self) -> String {
        match self {
            Advice::Escalate {
                question,
                probability,
                detail,
            } => format!("jev advisory [{question}] p={probability:.3}: {detail}"),
            Advice::NoSignal { .. } => String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noul(p: f64) -> Answer {
        Answer::Noul { noul: p }
    }

    #[test]
    fn above_threshold_escalates() {
        let a = Advice::from_noul("injection", &noul(0.91), 0.8);
        assert!(a.is_escalation());
        match a {
            Advice::Escalate { probability, .. } => assert!((probability - 0.91).abs() < 1e-9),
            other => panic!("expected Escalate, got {other:?}"),
        }
    }

    #[test]
    fn below_threshold_is_the_same_value_as_being_unavailable_for_control_flow() {
        let low = Advice::from_noul("injection", &noul(0.01), 0.8);
        let absent = Advice::unavailable("TYPESAFE_API_KEY is not set");
        // The contract: a consumer branching on `is_escalation` cannot tell
        // "jev says it is fine" from "jev never ran".
        assert!(!low.is_escalation());
        assert!(!absent.is_escalation());
        assert_eq!(low.render(), absent.render());
        assert_eq!(low.render(), "");
    }

    #[test]
    fn exactly_at_threshold_escalates() {
        assert!(Advice::from_noul("q", &noul(0.8), 0.8).is_escalation());
    }

    #[test]
    fn wrong_shape_is_no_signal_not_an_escalation() {
        let score = Answer::Score {
            score: 2.0,
            confidence: 0.9,
        };
        let a = Advice::from_noul("q", &score, 0.5);
        assert!(!a.is_escalation());
        match a {
            Advice::NoSignal {
                reason: NoSignalReason::WrongAnswerShape(_),
            } => {}
            other => panic!("expected WrongAnswerShape, got {other:?}"),
        }
    }

    #[test]
    fn out_of_range_or_nan_never_escalates() {
        for p in [f64::NAN, f64::INFINITY, -0.5, 1.5] {
            let a = Advice::from_noul("q", &noul(p), 0.0);
            assert!(!a.is_escalation(), "p={p} escalated");
        }
    }

    #[test]
    fn there_is_no_clean_variant() {
        // A reflective guard on the enum's own shape: if someone adds a third
        // variant, this match stops being exhaustive over the two and the test
        // body must be revisited. The named check is the intent.
        let a = Advice::unavailable("x");
        let shape = match a {
            Advice::Escalate { .. } => "escalate",
            Advice::NoSignal { .. } => "no-signal",
        };
        assert_eq!(shape, "no-signal");
    }
}
